//! The pack a sync sends: which objects go, and building it.
//!
//! `full` history sends everything reachable from the wanted commits that is
//! not reachable from a commit the sandbox is known to have (Phase 4's thin
//! pack). `head` history must not walk history at all. Excluding old commits
//! with plain negative revisions would be wrong there: a negative revision
//! tells Git the receiver has everything reachable from it, but a `head`
//! sandbox has only the trees of the commits it was sent, so objects shared
//! with their parents would be left out. Instead every commit involved is
//! registered as shallow for the walk (`--shallow <sha>` on pack-objects'
//! stdin, as `git upload-pack` does for shallow clients): HEAD's walk stops
//! at HEAD, so its complete tree is sent and no parent is, and each known
//! commit excludes exactly its own tree. `--shallow` on the command line
//! makes those known commits delta bases, so the pack stays thin.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

use super::{History, git};
use crate::error::AppError;

/// pack-objects' input for one pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revs {
    /// The lines written to `git pack-objects --revs` on stdin.
    pub text: String,
    /// Build with `--shallow` (a `head` pack).
    pub shallow: bool,
}

/// The objects `known` lists that the sandbox can be assumed to hold, given
/// the history mode recorded for it. A sandbox synced in `head` mode has no
/// history behind its commits, so a `full` sync (deepening it) assumes
/// nothing and sends all history once.
pub fn usable_known<'a>(
    history: History,
    recorded_mode: &str,
    known: &'a [String],
) -> &'a [String] {
    if history == History::Full && recorded_mode != History::Full.as_str() {
        &[]
    } else {
        known
    }
}

/// The rev list for a pack carrying `wanted` (the snapshot commit, HEAD,
/// then any mirrored refs) to a sandbox that holds `known`. `None` when the
/// sandbox already has every wanted object.
pub fn revs(
    root: &Path,
    history: History,
    wanted: &[String],
    known: &[String],
) -> Result<Option<Revs>, AppError> {
    let present = existing_objects(root, known)?;
    let mut seen = BTreeSet::new();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|sha| !present.contains_key(*sha) && seen.insert(sha.as_str()))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let mut text = String::new();
    match history {
        History::Full => {
            for sha in missing {
                text.push_str(&format!("{sha}\n"));
            }
            for sha in present.keys() {
                text.push_str(&format!("^{sha}\n"));
            }
        }
        History::Head => {
            // wanted[1] is HEAD: the boundary of what is sent.
            let commits: Vec<&String> = present
                .iter()
                .filter(|(_, kind)| kind.as_str() == "commit")
                .map(|(sha, _)| sha)
                .collect();
            let mut boundary: Vec<&String> = wanted.get(1).into_iter().collect();
            boundary.extend(commits.iter().copied());
            for sha in boundary {
                text.push_str(&format!("--shallow {sha}\n"));
            }
            for sha in missing {
                text.push_str(&format!("{sha}\n"));
            }
            for sha in commits {
                text.push_str(&format!("^{sha}\n"));
            }
        }
    }
    Ok(Some(Revs {
        text,
        shallow: history == History::Head,
    }))
}

/// The `known` objects that still exist locally, with their types (one git
/// process).
fn existing_objects(root: &Path, shas: &[String]) -> Result<BTreeMap<String, String>, AppError> {
    if shas.is_empty() {
        return Ok(BTreeMap::new());
    }
    let input: String = shas.iter().map(|sha| format!("{sha}\n")).collect();
    let output = git(
        root,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        &[],
        Some(input.as_bytes()),
        false,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(sha, kind)| {
            sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) && *kind != "missing"
        })
        .map(|(sha, kind)| (sha.to_string(), kind.to_string()))
        .collect())
}

/// Start `git pack-objects` for these revs, writing the pack to its stdout.
pub fn spawn(root: &Path, revs: &Revs) -> Result<Child, AppError> {
    let mut args = vec![
        "pack-objects",
        "--stdout",
        "--revs",
        "--thin",
        "--delta-base-offset",
        "-q",
    ];
    if revs.shallow {
        args.push("--shallow");
    }
    let mut child = std::process::Command::new("git")
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::io("cannot run git pack-objects", error))?;
    // pack-objects reads every rev before it writes, so this cannot deadlock.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(revs.text.as_bytes())
        .map_err(|error| AppError::io("cannot write to git pack-objects", error))?;
    drop(stdin);
    Ok(child)
}

/// Wait for pack-objects; its failure is an error.
pub fn finish(child: &mut Child) -> Result<(), AppError> {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child
        .wait()
        .map_err(|error| AppError::io("cannot wait for git pack-objects", error))?;
    if status.success() {
        return Ok(());
    }
    Err(AppError::new(
        "sync_failed",
        format!("git pack-objects failed: {}", stderr.trim()),
    ))
}

/// A built pack, small enough to keep in memory or spilled to disk.
#[derive(Debug)]
pub enum Built {
    Bytes(Vec<u8>),
    File(TempPack),
}

/// A pack file under the Git directory, removed on drop.
#[derive(Debug)]
pub struct TempPack {
    path: PathBuf,
    bytes: u64,
}

impl TempPack {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for TempPack {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Build the whole pack. Up to `threshold` bytes it stays in memory; a
/// larger pack is written to `<git_dir>/boatparty-pack-<nonce>.pack`.
pub fn build(
    root: &Path,
    git_dir: &Path,
    revs: &Revs,
    threshold: u64,
    nonce: &str,
) -> Result<Built, AppError> {
    let mut child = spawn(root, revs)?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut head = Vec::new();
    let read = (&mut stdout)
        .take(threshold.saturating_add(1))
        .read_to_end(&mut head);
    if let Err(error) = read {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::io("cannot read git pack-objects", error));
    }
    if head.len() as u64 <= threshold {
        drop(stdout);
        finish(&mut child)?;
        return Ok(Built::Bytes(head));
    }
    let path = git_dir.join(format!("boatparty-pack-{nonce}.pack"));
    let spilled = spill(&path, &head, &mut stdout);
    drop(stdout);
    let finished = finish(&mut child);
    match (spilled, finished) {
        (Ok(bytes), Ok(())) => Ok(Built::File(TempPack { path, bytes })),
        (spilled, finished) => {
            let _ = fs::remove_file(&path);
            finished?;
            Err(AppError::io(
                format!("cannot write {}", path.display()),
                spilled.err().unwrap_or_else(|| io::Error::other("unknown")),
            ))
        }
    }
}

fn spill(path: &Path, head: &[u8], rest: &mut impl Read) -> io::Result<u64> {
    let mut file = File::create(path)?;
    file.write_all(head)?;
    let copied = io::copy(rest, &mut file)?;
    file.flush()?;
    Ok(head.len() as u64 + copied)
}
