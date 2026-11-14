//! The working tree as a commit, built in a private index.
//!
//! `<git-dir>/boatparty-index` receives `git add -A`, so it holds tracked
//! plus untracked files minus ignored ones while the developer's real index
//! stays untouched. The commit is reused while HEAD and the tree are
//! unchanged (`<git-dir>/boatparty-wip`), so a repeated sync sends nothing.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{WorktreeInfo, git};
use crate::error::AppError;

const INDEX: &str = "boatparty-index";
const WIP_CACHE: &str = "boatparty-wip";
const LOCK: &str = "boatparty-sync.lock";

const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "boatparty"),
    ("GIT_AUTHOR_EMAIL", "boatparty@localhost"),
    ("GIT_COMMITTER_NAME", "boatparty"),
    ("GIT_COMMITTER_EMAIL", "boatparty@localhost"),
];

/// The working tree's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub tree: String,
    /// The snapshot commit, whose parent is HEAD.
    pub wip: String,
}

/// Snapshot the working tree of `info` into a commit on top of HEAD.
pub fn snapshot(info: &WorktreeInfo) -> Result<Snapshot, AppError> {
    // Overlapping syncs of one worktree share the private index.
    let lock_path = info.git_dir.join(LOCK);
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| AppError::io(format!("cannot open {}", lock_path.display()), error))?;
    lock.lock()
        .map_err(|error| AppError::io(format!("cannot lock {}", lock_path.display()), error))?;

    let index = info.git_dir.join(INDEX);
    if !index.exists() {
        // Start from the real index so its stat cache spares rehashing.
        let real = info.git_dir.join("index");
        if real.exists() {
            fs::copy(&real, &index)
                .map_err(|error| AppError::io(format!("cannot copy {}", real.display()), error))?;
        }
    }
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    git(&info.root, &["add", "-A"], &env, None, false)?;
    match_ignored_to_real_index(info, &index)?;
    let tree = text(git(&info.root, &["write-tree"], &env, None, false)?.stdout);

    let cache = info.git_dir.join(WIP_CACHE);
    let cached = fs::read_to_string(&cache).unwrap_or_default();
    let fields: Vec<&str> = cached.split_whitespace().collect();
    if let [head, cached_tree, wip] = fields[..]
        && head == info.head
        && cached_tree == tree
    {
        return Ok(Snapshot {
            tree,
            wip: wip.to_string(),
        });
    }
    let identity: Vec<(&str, &OsStr)> = IDENTITY
        .iter()
        .map(|(name, value)| (*name, OsStr::new(value)))
        .collect();
    let wip = text(
        git(
            &info.root,
            &[
                "commit-tree",
                &tree,
                "-p",
                &info.head,
                "-m",
                "boatparty snapshot",
            ],
            &identity,
            None,
            false,
        )?
        .stdout,
    );
    fs::write(&cache, format!("{} {tree} {wip}\n", info.head))
        .map_err(|error| AppError::io(format!("cannot write {}", cache.display()), error))?;
    Ok(Snapshot { tree, wip })
}

/// `git add -A` decides by the private index alone what counts as tracked,
/// so ignored paths need the real index's view: an untracked file that
/// became ignored after an earlier sync must stop leaving the machine, and a
/// file force-added to the real index must be synced although it is ignored.
fn match_ignored_to_real_index(info: &WorktreeInfo, index: &Path) -> Result<(), AppError> {
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    let ignored_cached = [
        "ls-files",
        "-z",
        "--cached",
        "--ignored",
        "--exclude-standard",
    ];
    let private = git(&info.root, &ignored_cached, &env, None, false)?.stdout;
    let real = if info.git_dir.join("index").exists() {
        git(&info.root, &ignored_cached, &[], None, false)?.stdout
    } else {
        Vec::new()
    };
    let private: HashSet<&[u8]> = nul_split(&private).collect();
    let real: HashSet<&[u8]> = nul_split(&real).collect();
    let drop: Vec<&[u8]> = private.difference(&real).copied().collect();
    let add: Vec<&[u8]> = real
        .difference(&private)
        .copied()
        .filter(|path| fs::symlink_metadata(info.root.join(OsStr::from_bytes(path))).is_ok())
        .collect();
    for (paths, command) in [
        (drop, &["rm", "-q", "--cached", "--ignore-unmatch"][..]),
        (add, &["add", "-f"][..]),
    ] {
        if paths.is_empty() {
            continue;
        }
        let mut args = vec!["--literal-pathspecs"];
        args.extend_from_slice(command);
        args.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
        git(&info.root, &args, &env, Some(&nul_join(&paths)), false)?;
    }
    Ok(())
}

fn nul_split(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|&b| b == 0).filter(|path| !path.is_empty())
}

fn nul_join(paths: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for path in paths {
        out.extend_from_slice(path);
        out.push(0);
    }
    out
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}
