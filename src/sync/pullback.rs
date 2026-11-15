//! Copy-back: applying a command's file changes (formatting, generated
//! code) to the local working tree.
//!
//! The patch is checked first. When the local copy changed the same lines
//! meanwhile, nothing is touched and the patch is kept under the Git
//! directory for the user to look at.

use std::fs;
use std::path::PathBuf;

use super::{WorktreeInfo, git};
use crate::error::AppError;

/// What happened to a command's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullBack {
    /// The changes are in the working tree now.
    Applied { files: Vec<String> },
    /// They conflict with local edits; nothing changed and the patch is kept.
    Conflict {
        files: Vec<String>,
        kept: PathBuf,
        reason: String,
    },
}

impl PullBack {
    pub fn files(&self) -> &[String] {
        match self {
            Self::Applied { files } | Self::Conflict { files, .. } => files,
        }
    }
}

/// Apply `patch` (from `git diff --binary`) to the working tree, or keep it at
/// `<git-dir>/boatparty-pullback-<ms>.patch` when it does not apply cleanly.
pub fn apply_pullback(info: &WorktreeInfo, patch: &[u8]) -> Result<PullBack, AppError> {
    let kept = info.git_dir.join(format!(
        "boatparty-pullback-{}.patch",
        crate::system::now_ms()
    ));
    fs::write(&kept, patch)
        .map_err(|error| AppError::io(format!("cannot write {}", kept.display()), error))?;
    let result = apply(info, &kept);
    if !matches!(result, Ok(PullBack::Conflict { .. })) {
        let _ = fs::remove_file(&kept);
    }
    result
}

fn apply(info: &WorktreeInfo, file: &std::path::Path) -> Result<PullBack, AppError> {
    let path = file.to_string_lossy().to_string();
    let numstat = git(&info.root, &["apply", "--numstat", &path], &[], None, false)?;
    let files: Vec<String> = String::from_utf8_lossy(&numstat.stdout)
        .lines()
        .filter_map(|line| line.splitn(3, '\t').nth(2))
        .map(str::to_string)
        .collect();
    let check = git(&info.root, &["apply", "--check", &path], &[], None, true)?;
    if !check.status.success() {
        return Ok(PullBack::Conflict {
            files,
            kept: file.to_path_buf(),
            reason: String::from_utf8_lossy(&check.stderr).trim().to_string(),
        });
    }
    git(&info.root, &["apply", &path], &[], None, false)?;
    Ok(PullBack::Applied { files })
}
