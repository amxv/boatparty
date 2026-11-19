//! `boatparty forward`: hold a local port forward to the sandbox until
//! cancelled, under a `session` lease (so the member stays up meanwhile).
//!
//! The forward travels on an SSH connection of its own, never the shared
//! control master: a forward added to the master would outlive this
//! process. The remote side of the connection only waits for end of file on
//! its stdin, which this process holds, so the connection (and with it the
//! forward) ends however this process ends, SIGKILL included.
//!
//! When the connection drops after the forward was up, it is opened again
//! (see `hold`), so a network gap or a sandbox restart only interrupts the
//! forward.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::json;

use super::session::{Held, with_session};
use crate::Context;
use crate::cli::ForwardArgs;
use crate::error::{AppError, EXIT_TOOL_FAILURE};
use crate::signals::Interrupts;
use crate::transport::forward::Forward;

/// What the remote side runs: say the forward is up, then wait for end of
/// file. With `ExitOnForwardFailure`, ssh sets local forwards up before it
/// runs this, so "ready" means the forward listens.
const REMOTE: &str = "echo ready; exec cat >/dev/null";
const READY: &str = "ready";
/// How long a closed connection may take to end before it is killed.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

pub fn run(ctx: &Context, args: &ForwardArgs) -> Result<u8, AppError> {
    let forward = Forward::local(args.bind.clone(), args.ports.local, args.ports.remote);
    forward.check_local_port()?;
    if !forward.is_loopback() {
        ctx.out.note(format_args!(
            "listening on {}, which other machines may reach",
            forward.bind
        ));
    }
    let label = format!("forward {forward}");
    let began = Instant::now();
    let ended = with_session(ctx, &label, |held| hold(ctx, held, &forward, began))?;
    let held = ended.value?;
    Ok(match (held, ended.interrupted) {
        // A cancel is how a forward normally ends.
        (_, Some(status)) => status,
        (Outcome::Cancelled(status), None) => status,
        (Outcome::Lost, None) => EXIT_TOOL_FAILURE,
    })
}

/// How holding the forward ended.
enum Outcome {
    /// A SIGINT or SIGTERM closed it; the exit status it asks for.
    Cancelled(u8),
    /// The connection ended by itself after the forward was up (already
    /// reported on stderr).
    Lost,
}

/// How long a lost forward keeps trying to reconnect before it gives up.
const RECONNECT_FOR: Duration = Duration::from_secs(10 * 60);
/// Overrides [`RECONNECT_FOR`] (seconds; 0 turns reconnecting off).
const RECONNECT_ENV: &str = "BOATPARTY_FORWARD_RECONNECT_SECONDS";
/// The first and the longest pause between reconnect attempts.
const FIRST_PAUSE: Duration = Duration::from_millis(500);
const LONGEST_PAUSE: Duration = Duration::from_secs(20);
/// Failed attempts in a row after which the member is brought up again
/// (it may have stopped or moved to a new machine).
const BRING_UP_AFTER: u32 = 3;

/// Hold the forward until a signal. When its connection drops after the
/// forward was up (a network gap, the sandbox restarting), connect again
/// with growing pauses, bringing the member up again when attempts keep
/// failing, for up to [`RECONNECT_FOR`].
fn hold(
    ctx: &Context,
    held: &Held<'_>,
    forward: &Forward,
    began: Instant,
) -> Result<Outcome, AppError> {
    let interrupts = held.interrupts;
    let reconnect_for = std::env::var(RECONNECT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(RECONNECT_FOR, Duration::from_secs);
    let mut announced = false;
    let mut lost_at: Option<Instant> = None;
    let mut failures = 0u32;
    let mut pause = FIRST_PAUSE;
    loop {
        let mut on_ready = || {
            if announced {
                ctx.out
                    .note(format_args!("reconnected; forwarding {forward} again"));
            } else {
                announced = true;
                announce(ctx, held, forward, began);
            }
        };
        let attempt = connect_once(held, forward, interrupts, &mut on_ready)?;
        match attempt {
            Attempt::Cancelled { status, ready } => {
                if !ready && !announced {
                    // The one document a JSON caller gets.
                    ctx.out.error(&AppError::new(
                        "interrupted",
                        "cancelled before the forward was up",
                    ));
                }
                return Ok(Outcome::Cancelled(status));
            }
            Attempt::Ended { ready, code } => {
                if !announced {
                    return Err(AppError::new(
                        "connection",
                        format!(
                            "the forward's SSH connection to {} ended before the forward was up ({code})",
                            held.member.name
                        ),
                    )
                    .with_hint("see ssh's message above; the local port may be taken or the sandbox stopped"));
                }
                if ready {
                    // It was up: start counting again.
                    lost_at = Some(Instant::now());
                    failures = 0;
                    pause = FIRST_PAUSE;
                    if reconnect_for.is_zero() {
                        ctx.out.note(format_args!(
                            "error: the forward's SSH connection to {} ended ({code}); the forward is closed",
                            held.member.name
                        ));
                        return Ok(Outcome::Lost);
                    }
                    ctx.out.note(format_args!(
                        "the forward's SSH connection to {} ended ({code}); reconnecting",
                        held.member.name
                    ));
                } else {
                    failures += 1;
                }
                let since = lost_at.get_or_insert_with(Instant::now);
                if since.elapsed() >= reconnect_for {
                    ctx.out.note(format_args!(
                        "error: could not reconnect to {} for {}s ({code}); the forward is closed",
                        held.member.name,
                        reconnect_for.as_secs()
                    ));
                    return Ok(Outcome::Lost);
                }
                if failures > 0 && failures.is_multiple_of(BRING_UP_AFTER) {
                    bring_up_again(ctx, held)?;
                }
            }
        }
        if let Some(status) = pause_for(pause, interrupts) {
            return Ok(Outcome::Cancelled(status));
        }
        pause = (pause * 2).min(LONGEST_PAUSE);
    }
}

/// One connection carrying the forward, and how it ended.
enum Attempt {
    /// A SIGINT or SIGTERM closed it.
    Cancelled { status: u8, ready: bool },
    /// It ended by itself; `ready` when the forward was up meanwhile.
    Ended { ready: bool, code: String },
}

/// Open one connection carrying the forward and hold it until it ends or a
/// signal arrives, calling `on_ready` once the forward listens.
fn connect_once(
    held: &Held<'_>,
    forward: &Forward,
    interrupts: &Interrupts,
    on_ready: &mut dyn FnMut(),
) -> Result<Attempt, AppError> {
    let mut command = held.target.ssh_dedicated();
    command
        .args(forward.ssh_args())
        .arg("-T")
        .arg(&held.target.alias)
        .arg(REMOTE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // Its own process group: Ctrl-C reaches this process, which closes the
    // connection itself.
    crate::system::separate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| AppError::io("cannot run ssh", error))?;
    let stdin = child.stdin.take();
    let (tx, rx) = mpsc::channel::<()>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if line.trim() == READY => {
                        let _ = tx.send(());
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }
    let mut ready = false;
    loop {
        if let Some(status) = interrupts.exit_status() {
            close(&mut child, stdin);
            return Ok(Attempt::Cancelled { status, ready });
        }
        match rx.recv_timeout(TICK) {
            Ok(()) if !ready => {
                ready = true;
                on_ready();
            }
            Ok(()) | Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            // A "ready" may have arrived just before the end.
            if !ready && rx.try_recv().is_ok() {
                ready = true;
                on_ready();
            }
            return Ok(Attempt::Ended {
                ready,
                code: describe(status),
            });
        }
    }
}

fn describe(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map_or("a signal".to_string(), |code| format!("exit {code}"))
}

/// Sleep `pause`, unless a signal arrives first.
fn pause_for(pause: Duration, interrupts: &Interrupts) -> Option<u8> {
    let until = Instant::now() + pause;
    while Instant::now() < until {
        if let Some(status) = interrupts.exit_status() {
            return Some(status);
        }
        std::thread::sleep(TICK.min(until.saturating_duration_since(Instant::now())));
    }
    interrupts.exit_status()
}

/// Bring this workspace's member up again (it may have stopped or moved to
/// a new machine) so the next attempt reaches it. The lease this process
/// holds keeps it up afterwards.
fn bring_up_again(ctx: &Context, held: &Held<'_>) -> Result<(), AppError> {
    let up = match super::WorkspaceContext::open(ctx)
        .and_then(|wctx| wctx.pool(ctx).bring_up(wctx.bring_up_options(true), None))
    {
        Ok(up) => up,
        Err(error) => {
            ctx.out.note(format_args!(
                "could not bring {} up again ({error}); still trying",
                held.member.name
            ));
            return Ok(());
        }
    };
    if up.member.id != held.member.id {
        return Err(AppError::new(
            "connection",
            format!(
                "{} is gone and this workspace now uses {}; run forward again",
                held.member.name, up.member.name
            ),
        ));
    }
    Ok(())
}

/// Close the connection by closing its stdin, which ends the remote side
/// and so the session; kill it if it lingers.
fn close(child: &mut Child, stdin: Option<ChildStdin>) {
    if let Some(mut stdin) = stdin {
        let _ = stdin.flush();
        drop(stdin);
    }
    let deadline = Instant::now() + CLOSE_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn announce(ctx: &Context, held: &Held<'_>, forward: &Forward, began: Instant) {
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": held.project,
            "member": {
                "name": held.member.name,
                "id": held.member.id,
            },
            "forward": {
                "bind": forward.bind,
                "listen": forward.listen,
                "target": forward.target,
            },
            "ready_seconds": (began.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
        }));
    }
    ctx.out.note(format_args!(
        "forwarding {forward} on {} ({}); Ctrl-C to stop",
        held.member.name,
        held.member.id.as_deref().unwrap_or_default()
    ));
}
