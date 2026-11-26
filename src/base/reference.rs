//! Which commit a base holds: `[base] ref`, else the default remote branch
//! (what `origin/HEAD` points at, else `origin/main`, else `origin/master`)
//! when the repository has one, else HEAD. Resolved locally; nothing is
//! fetched.

use crate::error::AppError;
use crate::sync::{WorktreeInfo, git};

/// The resolved base ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRef {
    /// The full ref name (`refs/remotes/origin/main`), `HEAD`, or the text
    /// configured when it names a commit directly.
    pub name: String,
    pub commit: String,
    /// The branch the base's checkout has checked out: the ref's branch
    /// name for a local or remote-tracking branch, the current branch for
    /// HEAD, none (detached) otherwise.
    pub branch: Option<String>,
}

/// Resolve the base ref of the repository `info` describes.
pub fn resolve(info: &WorktreeInfo, configured: Option<&str>) -> Result<BaseRef, AppError> {
    if let Some(wanted) = configured {
        let commit = commit_of(info, wanted).ok_or_else(|| {
            AppError::config(format!(
                "[base] ref {wanted:?} does not name a commit in {}",
                info.root.display()
            ))
            .with_hint("fetch it, or change [base] ref in boatparty.toml")
        })?;
        let full = text(info, &["rev-parse", "--symbolic-full-name", wanted]).unwrap_or_default();
        let name = if full.is_empty() {
            wanted.to_string()
        } else {
            full
        };
        let branch = if name == "HEAD" {
            info.branch.clone()
        } else {
            branch_of(&name)
        };
        return Ok(BaseRef {
            name,
            commit,
            branch,
        });
    }
    if let Some(name) = default_remote_branch(info)
        && let Some(commit) = commit_of(info, &name)
    {
        let branch = branch_of(&name);
        return Ok(BaseRef {
            name,
            commit,
            branch,
        });
    }
    Ok(BaseRef {
        name: "HEAD".to_string(),
        commit: info.head.clone(),
        branch: info.branch.clone(),
    })
}

/// The commit `name` points at now, if any.
pub fn commit_of(info: &WorktreeInfo, name: &str) -> Option<String> {
    let commit = text(
        info,
        &[
            "rev-parse",
            "-q",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )?;
    (!commit.is_empty()).then_some(commit)
}

pub(crate) fn default_remote_branch(info: &WorktreeInfo) -> Option<String> {
    let head = text(info, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]).unwrap_or_default();
    [
        head.as_str(),
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ]
    .into_iter()
    .filter(|name| !name.is_empty())
    .find(|name| commit_of(info, name).is_some())
    .map(str::to_string)
}

/// `refs/heads/<b>` and `refs/remotes/<remote>/<b>` name branch `<b>`.
fn branch_of(full: &str) -> Option<String> {
    if let Some(branch) = full.strip_prefix("refs/heads/") {
        return Some(branch.to_string());
    }
    let rest = full.strip_prefix("refs/remotes/")?;
    let (_, branch) = rest.split_once('/')?;
    (!branch.is_empty() && branch != "HEAD").then(|| branch.to_string())
}

fn text(info: &WorktreeInfo, args: &[&str]) -> Option<String> {
    let output = git(&info.root, args, &[], None, true).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::branch_of;

    #[test]
    fn branch_names_come_from_local_and_remote_branches_only() {
        assert_eq!(branch_of("refs/heads/main").as_deref(), Some("main"));
        assert_eq!(
            branch_of("refs/remotes/origin/feature/x").as_deref(),
            Some("feature/x")
        );
        assert_eq!(branch_of("refs/remotes/origin/HEAD"), None);
        assert_eq!(branch_of("refs/tags/v1"), None);
        assert_eq!(branch_of("HEAD"), None);
    }
}
