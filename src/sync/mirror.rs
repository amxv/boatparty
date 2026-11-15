//! Sending a snapshot to the runner, alone (`load`) or followed by a command
//! (`cheers`): the thin pack, streamed from `git pack-objects` straight into
//! the runner's stdin (or, above the host's upload threshold, uploaded by a
//! faster route and read from a file), the exit-96 install handshake, and
//! the one `needfull` retry with a pack that assumes nothing.

use std::io::Write;
use std::process::Stdio;

use super::exchange::{self, CancelProbe, Exchange, NeverCancel, PackInput, Sink, Spec};
use super::pack::{self, Built, Revs};
use super::seed::{self, SeedConfig, SeedReport, SeedRoute};
use super::{History, LocalRef, Snapshot, WorktreeInfo, refs_text};
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
    /// How much history to send.
    pub history: History,
    /// Seed from the GitHub remote when the sandbox holds nothing yet.
    pub seed: Option<&'a SeedConfig>,
}

/// How a sync's pack reached the sandbox.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PackRoute {
    /// Nothing needed sending.
    #[default]
    None,
    /// Over the runner's stdin.
    Ssh,
    /// Uploaded through a signed URL, then read from a file.
    SignedUrl,
}

impl PackRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ssh => "ssh",
            Self::SignedUrl => "signed_url",
        }
    }
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
    /// How the pack travelled.
    pub pack_route: PackRoute,
    /// Why a signed upload failed and the pack went over SSH instead.
    pub upload_fallback: Option<String>,
    /// What seeding from the GitHub remote did.
    pub seed: SeedReport,
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
            pack_route: session.pack_route,
            upload_fallback: session.upload_fallback,
            seed: session.seed,
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
    pub pack_route: PackRoute,
    pub upload_fallback: Option<String>,
    pub seed: SeedReport,
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
        pack_route: session.pack_route,
        upload_fallback: session.upload_fallback.clone(),
        seed: session.seed.clone(),
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
    pack_route: PackRoute,
    upload_fallback: Option<String>,
    seed: SeedReport,
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
    let mut full_retry = false;
    let mut pack_bytes = 0;
    let mut pack_route = PackRoute::None;
    let mut upload_fallback = None;
    let mut known: Vec<String> = request.known.to_vec();
    let seed = match request.seed {
        Some(config) if known.is_empty() => seed_first(host, request, config, sink),
        _ => SeedReport::default(),
    };
    known.extend(seed.commits.iter().cloned());
    let mut installed = seed.installed;
    let mut send_refs = request.send_refs;
    // A pack uploaded for an attempt that found the runner missing is still
    // on the sandbox, and the retry would send the same pack.
    let mut reuse: Option<String> = None;
    loop {
        let nonce = crate::system::random_hex(8)?;
        let Attempt {
            exchange,
            uploaded,
            route,
            fallback,
            upload_name,
        } = attempt(
            host,
            request,
            &nonce,
            Want {
                known: &known,
                send_refs,
                reuse: reuse.take(),
            },
            command,
            sink,
            cancel,
        )?;
        pack_bytes += exchange.pack_bytes + uploaded;
        if route != PackRoute::None {
            pack_route = route;
        }
        if fallback.is_some() {
            upload_fallback = fallback;
        }
        if exchange.cancelled {
            return Ok(Session {
                exchange,
                nonce,
                pack_bytes,
                full_retry,
                installed,
                pack_route,
                upload_fallback,
                seed,
            });
        }
        if exchange.runner_missing && !installed {
            host.install()?;
            installed = true;
            reuse = upload_name;
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
                    pack_route,
                    upload_fallback,
                    seed,
                });
            }
        }
    }
}

/// Seed a sandbox that holds nothing yet from the project's GitHub remote.
/// A failure is reported, never raised: the pack then carries everything.
fn seed_first(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    config: &SeedConfig,
    sink: &mut dyn Sink,
) -> SeedReport {
    let started = std::time::Instant::now();
    let mut report = SeedReport::default();
    if !config.enabled {
        report.skipped = Some("seeding is off ([sync] seed = false)".to_string());
        return report;
    }
    let plan = match seed::plan(request.info, request.history, request.refs, config) {
        Ok(plan) => plan,
        Err(reason) => {
            report.skipped = Some(reason);
            return report;
        }
    };
    let token = seed::token(&config.token, &plan.url);
    let fetched = seed::fetch(
        host,
        request.project,
        request.history,
        &plan,
        token.as_ref(),
        config.timeout_secs,
    );
    drop(token);
    report.millis = started.elapsed().as_millis() as u64;
    match fetched {
        Ok(installed) => {
            report.route = SeedRoute::Github;
            report.commits = plan.commits;
            report.installed = installed;
        }
        Err(reason) => {
            sink.note(&format!(
                "seeding from {} failed ({reason}); sending every object instead",
                plan.url
            ));
            report.fallback = Some(reason);
        }
    }
    report
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

    fn note(&mut self, message: &str) {
        let _ = writeln!(self.log, "boatparty: {message}");
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

/// What one attempt sends.
struct Want<'a> {
    known: &'a [String],
    send_refs: bool,
    /// An already uploaded pack (its file name) to use instead of a new one.
    reuse: Option<String>,
}

fn attempt(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    nonce: &str,
    want: Want<'_>,
    command: Option<&str>,
    sink: &mut dyn Sink,
    cancel: &dyn CancelProbe,
) -> Result<Attempt, AppError> {
    let route = match want.reuse {
        Some(name) => Routed {
            arg: format!("@{name}"),
            input: PackInput::None,
            route: PackRoute::SignedUrl,
            uploaded: 0,
            fallback: None,
            name: Some(name),
        },
        None => {
            let mut wanted = vec![request.snapshot.wip.clone(), request.info.head.clone()];
            wanted.extend(request.refs.iter().map(|entry| entry.sha.clone()));
            let revs = pack::revs(&request.info.root, request.history, &wanted, want.known)?;
            route_pack(host, request, nonce, revs)?
        }
    };
    let send_refs = want.send_refs;
    let mut args: Vec<String> = [
        if command.is_some() { "run" } else { "sync" },
        request.project,
        nonce,
        &route.arg,
        &request.snapshot.wip,
        &request.info.head,
        request.info.branch.as_deref().unwrap_or(""),
        if send_refs { "send" } else { "keep" },
        request.history.as_str(),
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
    let exchange = exchange::run(
        host,
        Spec {
            args,
            nonce,
            header,
            pack: route.input,
            root: &request.info.root,
            hold_stdin: command.is_some(),
        },
        sink,
        cancel,
    )?;
    Ok(Attempt {
        exchange,
        uploaded: route.uploaded,
        route: route.route,
        fallback: route.fallback,
        upload_name: route.name,
    })
}

struct Attempt {
    exchange: Exchange,
    uploaded: u64,
    route: PackRoute,
    fallback: Option<String>,
    upload_name: Option<String>,
}

/// How one attempt's pack travels.
struct Routed {
    /// The runner's pack argument: `0`, `1` (stdin) or `@<name>` (uploaded).
    arg: String,
    input: PackInput,
    route: PackRoute,
    uploaded: u64,
    fallback: Option<String>,
    /// The uploaded file's name on the sandbox.
    name: Option<String>,
}

/// Send a pack above the host's upload threshold by its bulk route, and
/// anything else (or a pack whose upload failed) over the runner's stdin.
fn route_pack(
    host: &dyn RunnerHost,
    request: &MirrorRequest<'_>,
    nonce: &str,
    revs: Option<Revs>,
) -> Result<Routed, AppError> {
    let stdin = |input| Routed {
        arg: "1".to_string(),
        input,
        route: PackRoute::Ssh,
        uploaded: 0,
        fallback: None,
        name: None,
    };
    let Some(revs) = revs else {
        return Ok(Routed {
            arg: "0".to_string(),
            input: PackInput::None,
            route: PackRoute::None,
            uploaded: 0,
            fallback: None,
            name: None,
        });
    };
    let Some(threshold) = host.upload_threshold() else {
        return Ok(stdin(PackInput::Stream(revs)));
    };
    let built = pack::build(
        &request.info.root,
        &request.info.git_dir,
        &revs,
        threshold,
        nonce,
    )?;
    let file = match built {
        Built::Bytes(bytes) => return Ok(stdin(PackInput::Bytes(bytes))),
        Built::File(file) => file,
    };
    let name = format!("{nonce}.pack");
    match host.upload(&name, file.path()) {
        Ok(()) => Ok(Routed {
            arg: format!("@{name}"),
            input: PackInput::None,
            route: PackRoute::SignedUrl,
            uploaded: file.bytes(),
            fallback: None,
            name: Some(name),
        }),
        Err(error) => Ok(Routed {
            fallback: Some(error.message().to_string()),
            ..stdin(PackInput::File(file))
        }),
    }
}
