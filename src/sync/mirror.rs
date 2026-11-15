//! Sending a snapshot to the runner, alone (`load`) or followed by a command
//! (`cheers`): the thin pack, streamed from `git pack-objects` straight into
//! the runner's stdin, the exit-96 install handshake, and the one `needfull`
//! retry with a pack that assumes nothing.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use super::exchange::{self, CancelProbe, Exchange, NeverCancel, Sink, Spec};
use super::{LocalRef, Snapshot, WorktreeInfo, git, refs_text};
use crate::error::AppError;
use crate::runner::{EXIT_MISSING, EXIT_NEEDFULL, RunnerHost, Trailer};

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
    let mut sink = LogSink { log };
    let session = session(host, request, None, &mut sink, &NeverCancel)?;
    match session.exchange.trailer {
        Some(Trailer::Done { tree, .. }) => Ok(MirrorOutcome {
            tree,
            pack_bytes: session.pack_bytes,
            full_retry: session.full_retry,
            installed: session.installed,
            known: confirmed(request),
        }),
        _ => Err(failed(&session.exchange)),
    }
}

/// How a command run through the runner ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// The runner reported the command's exit code.
    Done {
        exit_code: i32,
        tree: String,
        patch_bytes: u64,
    },
    /// The runner ended without a result; `status` is the exit of the
    /// connection (`Some(255)` when SSH itself failed).
    Lost { status: Option<i32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub end: RunEnd,
    /// The nonce of the invocation that ran, for `fetch-patch`.
    pub nonce: String,
    pub pack_bytes: u64,
    pub full_retry: bool,
    pub installed: bool,
    /// A cancel ended the command.
    pub cancelled: bool,
    /// The objects now confirmed present on the sandbox (empty unless the
    /// sync was confirmed).
    pub known: Vec<String>,
}

/// Sync the snapshot and run `command` (one shell line) in the checkout.
/// stdout, stderr and events go to `sink` as they arrive. The runner keeps
/// the command alive only while this side holds its stdin, which a cancel
/// closes.
pub fn run_command(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    command: &str,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<RunOutcome, AppError> {
    let session = session(host, request, Some(command), sink, cancel)?;
    let exchange = &session.exchange;
    let end = match &exchange.trailer {
        Some(Trailer::Done {
            exit_code,
            tree,
            patch_bytes,
        }) => RunEnd::Done {
            exit_code: *exit_code,
            tree: tree.clone(),
            patch_bytes: *patch_bytes,
        },
        Some(Trailer::NeedFull) if !exchange.cancelled => return Err(failed(exchange)),
        _ if exchange.runner_missing => return Err(failed(exchange)),
        _ => RunEnd::Lost {
            status: exchange.status,
        },
    };
    let known = match end {
        RunEnd::Done { .. } => confirmed(request),
        RunEnd::Lost { .. } => Vec::new(),
    };
    Ok(RunOutcome {
        end,
        nonce: session.nonce,
        pack_bytes: session.pack_bytes,
        full_retry: session.full_retry,
        installed: session.installed,
        cancelled: exchange.cancelled,
        known,
    })
}

/// Print what a run left on the sandbox as a patch, and delete it there.
pub fn fetch_patch(host: &dyn RunnerHost, project: &str, nonce: &str) -> Result<Vec<u8>, AppError> {
    let args: Vec<String> = ["fetch-patch", project, nonce].map(String::from).to_vec();
    let output = host
        .command(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AppError::io("cannot start the runner", error))?;
    if !output.status.success() {
        return Err(AppError::new(
            "ssh",
            format!(
                "fetching the command's changes failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output.stdout)
}

struct Session {
    exchange: Exchange,
    nonce: String,
    pack_bytes: u64,
    full_retry: bool,
    installed: bool,
}

/// Invoke the runner until it answers: install it once when it is missing,
/// and send a full pack once when the thin one assumed too much. Neither
/// retry can repeat a command, because the runner reports both before the
/// command starts.
fn session(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    command: Option<&str>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<Session, AppError> {
    let mut installed = false;
    let mut full_retry = false;
    let mut pack_bytes = 0;
    let mut known: Vec<String> = request.known.to_vec();
    let mut send_refs = request.send_refs;
    loop {
        let nonce = crate::system::random_hex(8)?;
        let exchange = attempt(
            host, request, &nonce, &known, send_refs, command, sink, cancel,
        )?;
        pack_bytes += exchange.pack_bytes;
        if exchange.cancelled {
            return Ok(Session {
                exchange,
                nonce,
                pack_bytes,
                full_retry,
                installed,
            });
        }
        if exchange.runner_missing && !installed {
            host.install()?;
            installed = true;
            continue;
        }
        match exchange.trailer {
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
            _ => {
                return Ok(Session {
                    exchange,
                    nonce,
                    pack_bytes,
                    full_retry,
                    installed,
                });
            }
        }
    }
}

/// Runner stdout to this process's stderr, runner stderr to the log.
struct LogSink<'a> {
    log: &'a mut dyn Write,
}

impl Sink for LogSink<'_> {
    fn stdout(&mut self, bytes: &[u8]) {
        let _ = std::io::stderr().lock().write_all(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        let _ = self.log.write_all(bytes);
        let _ = self.log.flush();
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
        Some(EXIT_NEEDFULL) => AppError::new(
            "sync_failed",
            "the sandbox lacked objects the pack assumed it had",
        ),
        Some(code) => AppError::new(
            "sync_failed",
            format!("the runner exited {code} without reporting a result"),
        ),
        None => AppError::new("sync_failed", "the runner was killed by a signal"),
    }
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

#[allow(clippy::too_many_arguments)]
fn attempt(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    nonce: &str,
    known: &[String],
    send_refs: bool,
    command: Option<&str>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<Exchange, AppError> {
    let mut wanted = vec![request.snapshot.wip.clone(), request.info.head.clone()];
    wanted.extend(request.refs.iter().map(|entry| entry.sha.clone()));
    let revs = pack_revs(&request.info.root, &wanted, known)?;
    let mut args: Vec<String> = [
        if command.is_some() { "run" } else { "sync" },
        request.project,
        nonce,
        if revs.is_some() { "1" } else { "0" },
        &request.snapshot.wip,
        &request.info.head,
        request.info.branch.as_deref().unwrap_or(""),
        if send_refs { "send" } else { "keep" },
    ]
    .map(String::from)
    .to_vec();
    if let Some(command) = command {
        args.push(command.to_string());
    }
    let mut header = if send_refs {
        refs_text(request.refs).into_bytes()
    } else {
        Vec::new()
    };
    header.push(b'\n');
    exchange::run(
        host,
        Spec {
            args,
            nonce,
            header,
            revs,
            root: &request.info.root,
            hold_stdin: command.is_some(),
        },
        sink,
        cancel,
    )
}
