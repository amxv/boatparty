//! The local half of the Git-native mirror.
//!
//! The working tree (tracked plus untracked, minus ignored) is snapshotted
//! into a private index, so the developer's real index is never touched. The
//! snapshot becomes a commit on top of HEAD, and its objects travel to the
//! runner as a thin pack holding only what the sandbox lacks.
//!
//! How much history travels is the project's `[sync] history`: `head` (the
//! default) sends HEAD's commit and tree and no older history, and the
//! sandbox records HEAD as a shallow boundary; `full` also sends every local
//! branch, tag and the default remote branch with their history.

pub mod credentials;
mod detached;
pub mod exchange;
pub mod hold;
pub mod mirror;
pub mod pack;
pub mod pipe;
pub mod pullback;
pub mod seed;
pub mod snapshot;

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::AppError;

pub use credentials::RunCredentials;
pub use exchange::{CancelProbe, CancelState, NeverCancel, Sink};
pub use hold::{Hold, hold};
pub use mirror::{
    FetchedPatch, MirrorOutcome, MirrorRequest, PackRoute, RunEnd, RunOutcome, fetch_patch,
    fetch_patch_routed, mirror, run_command, run_command_with, serve_command_with,
};
pub use pipe::{PipeOutcome, pipe_command};
pub use pullback::{PullBack, apply_pullback};
pub use seed::{SeedConfig, SeedReport, SeedRoute, TokenSource};
pub use snapshot::{Snapshot, snapshot};

/// How much Git history a sync sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum History {
    /// HEAD's commit and tree, the snapshot, and the checked-out branch name.
    #[default]
    Head,
    /// Every local branch and tag and the default remote branch, with history.
    Full,
}

impl History {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Full => "full",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "head" => Ok(Self::Head),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "{other:?} is not a history mode; use \"head\" or \"full\""
            )),
        }
    }
}

/// The refs a sync mirrors besides the checked-out branch, and the digest
/// stored so unchanged refs are not re-sent. In `head` mode the list is
/// empty, so the sandbox keeps only the checked-out branch; its digest names
/// the mode and branch, so a switch of either prunes refs once.
pub fn mirrored_refs(
    info: &WorktreeInfo,
    history: History,
) -> Result<(Vec<LocalRef>, String), AppError> {
    match history {
        History::Full => {
            let refs = local_refs(info)?;
            let digest = refs_digest(&refs);
            Ok((refs, digest))
        }
        History::Head => {
            let label = format!("head {}\n", info.branch.as_deref().unwrap_or(""));
            Ok((Vec::new(), crate::runner::content_hash(label.as_bytes())))
        }
    }
}

/// Run git in `dir` with extra environment and optional stdin. A non-zero
/// exit is an error unless `allow_failure`.
pub(crate) fn git(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &OsStr)],
    stdin: Option<&[u8]>,
    allow_failure: bool,
) -> Result<Output, AppError> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(|error| {
        AppError::io("cannot run git", error).with_hint("install Git and run `boatparty doctor`")
    })?;
    // Write stdin from a thread so a large answer cannot deadlock the pipes.
    let writer = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => {
            let bytes = bytes.to_vec();
            Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            }))
        }
        _ => None,
    };
    let output = child
        .wait_with_output()
        .map_err(|error| AppError::io("cannot run git", error))?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    if !output.status.success() && !allow_failure {
        return Err(AppError::new(
            "sync_failed",
            format!(
                "git {} failed (exit {}): {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output)
}

/// git's trimmed stdout.
pub(crate) fn git_text(dir: &Path, args: &[&str]) -> Result<String, AppError> {
    let output = git(dir, args, &[], None, false)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What the mirror needs to know about the local checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// The Git toplevel.
    pub root: PathBuf,
    /// This worktree's own Git directory (per worktree, not the common one).
    pub git_dir: PathBuf,
    /// The HEAD commit.
    pub head: String,
    /// The checked-out branch, without `refs/heads/`; `None` when detached.
    pub branch: Option<String>,
}

impl WorktreeInfo {
    /// Read the checkout at `dir`. Not a Git repository, or a repository
    /// without a commit, is an error.
    pub fn read(dir: &Path) -> Result<Self, AppError> {
        let paths = git(
            dir,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
            ],
            &[],
            None,
            true,
        )?;
        let text = String::from_utf8_lossy(&paths.stdout).to_string();
        let mut lines = text.lines().filter(|_| paths.status.success());
        let (Some(root), Some(git_dir)) = (lines.next(), lines.next()) else {
            return Err(AppError::new(
                "workspace",
                format!(
                    "{} is not inside a Git working tree; boatparty works on Git checkouts only",
                    dir.display()
                ),
            ));
        };
        let (root, git_dir) = (PathBuf::from(root), PathBuf::from(git_dir));
        let head = git(
            &root,
            &["rev-parse", "-q", "--verify", "HEAD^{commit}"],
            &[],
            None,
            true,
        )?;
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        if head.is_empty() {
            return Err(AppError::new(
                "workspace",
                format!(
                    "{} has no commit yet; commit once before syncing",
                    root.display()
                ),
            ));
        }
        let branch = git(&root, &["symbolic-ref", "-q", "HEAD"], &[], None, true)?;
        let branch = String::from_utf8_lossy(&branch.stdout)
            .trim()
            .strip_prefix("refs/heads/")
            .map(str::to_string);
        Ok(Self {
            root,
            git_dir,
            head,
            branch,
        })
    }
}

/// One mirrored ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRef {
    pub sha: String,
    pub name: String,
}

/// Every local branch and tag except the checked-out branch (whose ref
/// follows HEAD), plus the default remote branch: what `origin/HEAD` points
/// at, else `origin/main`, else `origin/master`.
pub fn local_refs(info: &WorktreeInfo) -> Result<Vec<LocalRef>, AppError> {
    let text = git_text(
        &info.root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname) %(symref)",
            "refs/heads",
            "refs/tags",
            "refs/remotes/origin",
        ],
    )?;
    let current = info
        .branch
        .as_ref()
        .map(|branch| format!("refs/heads/{branch}"));
    let mut refs = Vec::new();
    let mut remotes = Vec::new();
    let mut origin_head = None;
    for line in text.lines() {
        let mut fields = line.split(' ');
        let (Some(sha), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        let symref = fields.next().unwrap_or("");
        let entry = LocalRef {
            sha: sha.to_string(),
            name: name.to_string(),
        };
        if name == "refs/remotes/origin/HEAD" {
            if !symref.is_empty() {
                origin_head = Some(symref.to_string());
            }
        } else if name.starts_with("refs/remotes/") {
            remotes.push(entry);
        } else if Some(name) != current.as_deref() {
            refs.push(entry);
        }
    }
    let default = origin_head
        .into_iter()
        .chain(["refs/remotes/origin/main", "refs/remotes/origin/master"].map(String::from))
        .find_map(|wanted| remotes.iter().find(|entry| entry.name == wanted).cloned());
    refs.extend(default);
    Ok(refs)
}

/// The refs as the runner reads them: `<sha> <ref>` lines.
pub fn refs_text(refs: &[LocalRef]) -> String {
    refs.iter()
        .map(|entry| format!("{} {}\n", entry.sha, entry.name))
        .collect()
}

/// A digest of the mirrored refs, stored so unchanged refs are not re-sent.
pub fn refs_digest(refs: &[LocalRef]) -> String {
    crate::runner::content_hash(refs_text(refs).as_bytes())
}
