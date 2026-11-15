//! `boatparty forward`: hold a local port forward to the sandbox until
//! cancelled, under a `session` lease (so the member stays up meanwhile).
//!
//! The forward travels on an SSH connection of its own, never the shared
//! control master: a forward added to the master would outlive this
//! process. The remote side of the connection only waits for end of file on
//! its stdin, which this process holds, so the connection (and with it the
//! forward) ends however this process ends, SIGKILL included.

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

fn hold(
    ctx: &Context,
    held: &Held<'_>,
    forward: &Forward,
    began: Instant,
) -> Result<Outcome, AppError> {
    let interrupts = Interrupts::install()?;
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
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
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
            if !ready {
                // The one document a JSON caller gets.
                ctx.out.error(&AppError::new(
                    "interrupted",
                    "cancelled before the forward was up",
                ));
            }
            return Ok(Outcome::Cancelled(status));
        }
        match rx.recv_timeout(TICK) {
            Ok(()) if !ready => {
                ready = true;
                announce(ctx, held, forward, began);
            }
            Ok(()) | Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            let code = status
                .code()
                .map_or("a signal".to_string(), |code| format!("exit {code}"));
            if !ready {
                return Err(AppError::new(
                    "connection",
                    format!(
                        "the forward's SSH connection to {} ended before the forward was up ({code})",
                        held.member.name
                    ),
                )
                .with_hint("see ssh's message above; the local port may be taken or the sandbox stopped"));
            }
            ctx.out.note(format_args!(
                "error: the forward's SSH connection to {} ended ({code}); the forward is closed",
                held.member.name
            ));
            return Ok(Outcome::Lost);
        }
    }
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
