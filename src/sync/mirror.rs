//! Sending a snapshot to the runner: the thin pack, streamed from
//! `git pack-objects` straight into the runner's stdin, the exit-96 install
//! handshake, and the one `needfull` retry with a pack that assumes nothing.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread::JoinHandle;

use super::{LocalRef, Snapshot, WorktreeInfo, git, refs_text};
use crate::error::AppError;
use crate::runner::{
    EXIT_MISSING, EXIT_NEEDFULL, MISSING_MARKER, RunnerHost, Trailer, TrailerFilter,
};

/// One sync of a snapshot to a runner.
pub struct MirrorRequest<'a> {
    pub project: &'a str,
    pub info: &'a WorktreeInfo,
    pub snapshot: &'a Snapshot,
    pub refs: &'a [LocalRef],
    /// Send the ref list; when false the sandbox keeps the refs it has.
    pub send_refs: bool,
    /// Objects the sandbox is believed to have.
    pub known: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorOutcome {
    /// The tree the sandbox's checkout now holds.
    pub tree: String,
    /// Pack bytes sent, over all attempts.
    pub pack_bytes: u64,
    /// The sandbox lacked objects the first pack assumed, so a full pack was sent.
    pub full_retry: bool,
    /// The runner was uploaded first.
    pub installed: bool,
    /// The objects now confirmed present on the sandbox.
    pub known: Vec<String>,
}

/// Mirror the snapshot to the runner. Runner stderr (minus its frames) is
/// copied to `log`; remote stdout goes to this process's stderr.
pub fn mirror(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    log: &mut dyn Write,
) -> Result<MirrorOutcome, AppError> {
    let mut installed = false;
    let mut full_retry = false;
    let mut pack_bytes = 0;
    let mut known: Vec<String> = request.known.to_vec();
    let mut send_refs = request.send_refs;
    loop {
        let exchange = attempt(host, request, &known, send_refs, log)?;
        pack_bytes += exchange.pack_bytes;
        if exchange.runner_missing && !installed {
            host.install()?;
            installed = true;
            continue;
        }
        match exchange.trailer {
            Some(Trailer::Done { tree, .. }) => {
                return Ok(MirrorOutcome {
                    tree,
                    pack_bytes,
                    full_retry,
                    installed,
                    known: confirmed(request),
                });
            }
            Some(Trailer::NeedFull) if !full_retry => {
                full_retry = true;
                known.clear();
                send_refs = true;
            }
            Some(Trailer::NeedFull) => {
                return Err(AppError::new(
                    "sync_failed",
                    "the sandbox could not take in a full pack of this repository",
                )
                .with_hint("see ~/.boatparty/out/*.pack-error on the sandbox"));
            }
            None => return Err(failed(&exchange)),
        }
    }
}

/// Everything the snapshot names, now on the sandbox.
fn confirmed(request: &MirrorRequest<'_>) -> Vec<String> {
    let mut known = vec![request.snapshot.wip.clone(), request.info.head.clone()];
    for entry in request.refs {
        if !known.contains(&entry.sha) {
            known.push(entry.sha.clone());
        }
    }
    known
}

fn failed(exchange: &Exchange) -> AppError {
    match exchange.status {
        Some(255) => AppError::new(
            "connection",
            "the SSH connection to the sandbox failed before the sync finished",
        ),
        Some(EXIT_MISSING) if exchange.runner_missing => AppError::new(
            "sync_failed",
            "the runner is still missing on the sandbox after uploading it",
        ),
        Some(code) => AppError::new(
            "sync_failed",
            format!("the runner exited {code} without reporting a result"),
        ),
        None => AppError::new("sync_failed", "the runner was killed by a signal"),
    }
}

#[derive(Debug)]
struct Exchange {
    status: Option<i32>,
    trailer: Option<Trailer>,
    runner_missing: bool,
    pack_bytes: u64,
}

/// The pack's rev list: the wanted objects the sandbox lacks, minus what it
/// has. `None` when it already has every wanted object.
fn pack_revs(root: &Path, wanted: &[String], known: &[String]) -> Result<Option<String>, AppError> {
    let present = existing_objects(root, known)?;
    let mut seen = BTreeSet::new();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|sha| !present.contains(*sha) && seen.insert(sha.as_str()))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let mut revs = String::new();
    for sha in missing {
        revs.push_str(sha);
        revs.push('\n');
    }
    for sha in &present {
        revs.push('^');
        revs.push_str(sha);
        revs.push('\n');
    }
    Ok(Some(revs))
}

/// The `known` objects that still exist locally (one git process).
fn existing_objects(root: &Path, shas: &[String]) -> Result<BTreeSet<String>, AppError> {
    if shas.is_empty() {
        return Ok(BTreeSet::new());
    }
    let input: String = shas.iter().map(|sha| format!("{sha}\n")).collect();
    let output = git(
        root,
        &["cat-file", "--batch-check=%(objectname)"],
        &[],
        Some(input.as_bytes()),
        false,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.len() >= 40 && line.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
        .collect())
}

fn attempt(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    known: &[String],
    send_refs: bool,
    log: &mut dyn Write,
) -> Result<Exchange, AppError> {
    let mut wanted = vec![request.snapshot.wip.clone(), request.info.head.clone()];
    wanted.extend(request.refs.iter().map(|entry| entry.sha.clone()));
    let revs = pack_revs(&request.info.root, &wanted, known)?;
    let nonce = crate::system::random_hex(8)?;
    let args: Vec<String> = [
        "sync",
        request.project,
        &nonce,
        if revs.is_some() { "1" } else { "0" },
        &request.snapshot.wip,
        &request.info.head,
        request.info.branch.as_deref().unwrap_or(""),
        if send_refs { "send" } else { "keep" },
    ]
    .map(String::from)
    .to_vec();
    let mut header = if send_refs {
        refs_text(request.refs).into_bytes()
    } else {
        Vec::new()
    };
    header.push(b'\n');

    let mut pack = match &revs {
        Some(revs) => Some(spawn_pack_objects(&request.info.root, revs)?),
        None => None,
    };
    // The runner prints nothing on stdout in sync mode; whatever it does
    // print must not mix with this command's own stdout.
    let remote_stdout = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|error| AppError::io("cannot duplicate stderr", error))?;
    let mut command = host.command(&args);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(remote_stdout))
        .stderr(Stdio::piped());
    let mut runner = match command.spawn() {
        Ok(runner) => runner,
        Err(error) => {
            if let Some(mut child) = pack {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(AppError::io("cannot start the runner", error));
        }
    };
    let runner_stdin = runner.stdin.take().expect("stdin is piped");
    let pack_stdout = pack.as_mut().and_then(|child| child.stdout.take());
    let writer = spawn_writer(runner_stdin, header, pack_stdout);

    let mut filter = TrailerFilter::new(&nonce);
    let mut sniffer = MissingSniffer::default();
    let mut stderr = runner.stderr.take().expect("stderr is piped");
    let mut buffer = [0u8; 8192];
    loop {
        let read = match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(AppError::io("cannot read the runner's output", error)),
        };
        let printable = filter.push(&buffer[..read]);
        let _ = log.write_all(&sniffer.pass(&printable));
    }
    let mut tail = sniffer.pass(&filter.end());
    tail.extend(sniffer.finish());
    let _ = log.write_all(&tail);
    let _ = log.flush();

    let status = runner
        .wait()
        .map_err(|error| AppError::io("cannot wait for the runner", error))?;
    let pack_bytes = writer.join().unwrap_or(0);
    let trailer = filter.result();
    let status = status.code();
    if let Some(mut child) = pack {
        let pack_status = finish_pack(&mut child);
        // A runner that stopped reading early (needfull, missing runner)
        // makes pack-objects fail on a closed pipe; only a pack that the
        // runner accepted needs to have been built successfully.
        if let (Err(error), Some(Trailer::Done { .. })) = (pack_status, &trailer) {
            return Err(error);
        }
    }
    Ok(Exchange {
        runner_missing: status == Some(EXIT_MISSING) && sniffer.matched,
        status: if trailer == Some(Trailer::NeedFull) {
            Some(EXIT_NEEDFULL)
        } else {
            status
        },
        trailer,
        pack_bytes,
    })
}

fn spawn_pack_objects(root: &Path, revs: &str) -> Result<Child, AppError> {
    let mut child = std::process::Command::new("git")
        .args([
            "pack-objects",
            "--stdout",
            "--revs",
            "--thin",
            "--delta-base-offset",
            "-q",
        ])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::io("cannot run git pack-objects", error))?;
    // pack-objects reads every rev before it writes, so this cannot deadlock.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(revs.as_bytes())
        .map_err(|error| AppError::io("cannot write to git pack-objects", error))?;
    drop(stdin);
    Ok(child)
}

fn finish_pack(child: &mut Child) -> Result<(), AppError> {
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

/// Write the header, then stream the pack, into the runner. Returns the
/// pack bytes written. A runner that stops reading ends the copy quietly;
/// its trailer says what happened.
fn spawn_writer(
    mut stdin: ChildStdin,
    header: Vec<u8>,
    pack: Option<ChildStdout>,
) -> JoinHandle<u64> {
    std::thread::spawn(move || {
        if stdin.write_all(&header).is_err() {
            return 0;
        }
        let Some(mut pack) = pack else {
            return 0;
        };
        let mut written = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = match pack.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if stdin.write_all(&buffer[..read]).is_err() {
                break;
            }
            written += read as u64;
        }
        written
    })
}

/// Holds back the start of the runner's stderr while it could still be the
/// missing-runner marker, so that marker is never shown.
#[derive(Debug, Default)]
struct MissingSniffer {
    held: Vec<u8>,
    decided: bool,
    matched: bool,
}

impl MissingSniffer {
    fn pass(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.decided {
            return bytes.to_vec();
        }
        self.held.extend_from_slice(bytes);
        let marker = format!("{MISSING_MARKER}\n").into_bytes();
        let n = self.held.len().min(marker.len());
        if self.held[..n] != marker[..n] {
            self.decided = true;
            return std::mem::take(&mut self.held);
        }
        if self.held.len() >= marker.len() {
            self.decided = true;
            self.matched = true;
            let rest = self.held.split_off(marker.len());
            self.held.clear();
            return rest;
        }
        Vec::new()
    }

    fn finish(&mut self) -> Vec<u8> {
        self.decided = true;
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::MissingSniffer;

    #[test]
    fn the_missing_marker_is_swallowed_and_other_output_is_not() {
        let mut sniffer = MissingSniffer::default();
        let mut out = sniffer.pass(b"boatparty-run");
        out.extend(sniffer.pass(b"ner-missing\n"));
        out.extend(sniffer.finish());
        assert!(out.is_empty());
        assert!(sniffer.matched);

        let mut sniffer = MissingSniffer::default();
        let mut out = sniffer.pass(b"boat");
        out.extend(sniffer.pass(b"s ahoy\n"));
        out.extend(sniffer.finish());
        assert_eq!(out, b"boats ahoy\n");
        assert!(!sniffer.matched);
    }
}
