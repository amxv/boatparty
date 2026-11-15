//! How fresh a base is, judged locally: its age, how many commits the base
//! ref gained since the base commit, and whether any `[base] stale_inputs`
//! file differs between the two. Nothing is fetched and nothing is rebuilt
//! automatically; `base status`, `status` and `init` report it.

use serde::Serialize;

use super::BaseRecord;
use super::reference;
use crate::sync::{WorktreeInfo, git};

/// A base this many commits behind its ref is reported stale.
pub const STALE_COMMITS: u64 = 50;
/// A base this old is reported stale.
pub const STALE_AGE_MS: u64 = 7 * 24 * 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Staleness {
    pub age_ms: u64,
    /// The base ref now (full name) and the commit it points at, when it
    /// resolves.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub ref_commit: Option<String>,
    /// Commits reachable from the ref but not from the base commit; `None`
    /// when this repository lacks the base commit or the ref.
    pub behind: Option<u64>,
    /// The `stale_inputs` files that differ between the base commit and the
    /// ref; `None` when that cannot be told.
    pub inputs_changed: Option<Vec<String>>,
    pub stale: bool,
    pub reasons: Vec<String>,
}

/// Judge `base` against the repository `info` describes now.
pub fn assess(
    info: &WorktreeInfo,
    base: &BaseRecord,
    configured_ref: Option<&str>,
    stale_inputs: &[String],
    now: u64,
) -> Staleness {
    let age_ms = now.saturating_sub(base.built_at);
    let current = reference::resolve(info, configured_ref).ok();
    let have_base = reference::commit_of(info, &base.commit).is_some();
    let (behind, inputs_changed) = match (&current, have_base) {
        (Some(current), true) => (
            count(info, &format!("{}..{}", base.commit, current.commit)),
            changed(info, &base.commit, &current.commit, stale_inputs),
        ),
        _ => (None, None),
    };
    let reasons = reasons(
        age_ms,
        behind,
        inputs_changed.as_deref(),
        current.as_ref().map(|current| current.name.as_str()),
        have_base,
    );
    Staleness {
        age_ms,
        git_ref: current.as_ref().map(|current| current.name.clone()),
        ref_commit: current.map(|current| current.commit),
        behind,
        inputs_changed,
        stale: !reasons.is_empty(),
        reasons,
    }
}

/// Why a base counts as stale; empty when it does not.
pub fn reasons(
    age_ms: u64,
    behind: Option<u64>,
    inputs_changed: Option<&[String]>,
    git_ref: Option<&str>,
    have_base: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(files) = inputs_changed
        && !files.is_empty()
    {
        reasons.push(format!("{} changed since the base", files.join(", ")));
    }
    if let Some(behind) = behind
        && behind >= STALE_COMMITS
    {
        reasons.push(format!(
            "{behind} commits behind {}",
            short_ref(git_ref.unwrap_or("its ref"))
        ));
    }
    if !have_base {
        reasons.push("this repository does not have the base commit".to_string());
    }
    if age_ms >= STALE_AGE_MS {
        reasons.push(format!("{} days old", age_ms / (24 * 3_600_000)));
    }
    reasons
}

/// `refs/remotes/origin/main` reads as `origin/main`.
pub fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/remotes/")
        .or_else(|| name.strip_prefix("refs/heads/"))
        .or_else(|| name.strip_prefix("refs/tags/"))
        .unwrap_or(name)
}

fn count(info: &WorktreeInfo, range: &str) -> Option<u64> {
    let output = git(&info.root, &["rev-list", "--count", range], &[], None, true).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn changed(info: &WorktreeInfo, from: &str, to: &str, files: &[String]) -> Option<Vec<String>> {
    if files.is_empty() {
        return Some(Vec::new());
    }
    let mut args = vec!["diff", "--name-only", from, to, "--"];
    args.extend(files.iter().map(String::as_str));
    let output = git(&info.root, &args, &[], None, true).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_base_has_no_reasons() {
        assert!(reasons(1000, Some(3), Some(&[]), Some("HEAD"), true).is_empty());
    }

    #[test]
    fn each_reason_is_named() {
        let files = vec!["Cargo.lock".to_string()];
        assert_eq!(
            reasons(0, Some(0), Some(&files), None, true),
            ["Cargo.lock changed since the base"]
        );
        assert_eq!(
            reasons(
                0,
                Some(STALE_COMMITS),
                None,
                Some("refs/remotes/origin/main"),
                true
            ),
            [format!("{STALE_COMMITS} commits behind origin/main")]
        );
        assert_eq!(
            reasons(STALE_AGE_MS + 1, None, None, None, true),
            ["7 days old"]
        );
        assert_eq!(
            reasons(0, None, None, None, false),
            ["this repository does not have the base commit"]
        );
    }
}
