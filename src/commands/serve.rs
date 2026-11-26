//! `boatparty serve`: a long-lived process, such as a dev server, run like
//! `run` (sync, credentials, [run.env], steps, live output, exit code)
//! until it is cancelled.
//!
//! What differs from `run`, and lives here:
//!
//! - The lease is `serve`, and its heartbeat runs from the start, so the
//!   member stays up for as long as the server does.
//! - The member is always brought up through the provider (no warm path),
//!   which also says whether its files are still being restored; the server
//!   starts only once they are.
//! - The server's invocation travels on an SSH connection of its own that
//!   carries its forwards, so they end with it and its end never touches the
//!   shared connection other commands use.
//! - Stopping: SIGINT or SIGTERM, the time cap, or this process dying in any
//!   way (the connection then closes, the runner sees end of file on its
//!   stdin and terminates the server's process group).
//! - Copy-back: like `run`, but only when the server exits by itself. A
//!   stopped server sends nothing back (the runner skips the patch of a
//!   cancelled command), so a server killed mid-write cannot overwrite
//!   local files; the next sync replaces what it left in the checkout.

use std::cell::Cell;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Context;
use crate::cli::ServeArgs;
use crate::error::AppError;
use crate::pool::wait::{self, Deadline};
use crate::provider::{Hydration, SandboxState};
use crate::signals::Interrupts;
use crate::stream::Stream;
use crate::sync::{CancelProbe, CancelState};
use crate::transport::forward::{Direction, Forward};

/// How long a server waits for its member's files before starting anyway.
const HYDRATION_WAIT: Duration = Duration::from_secs(30 * 60);
/// Overrides [`HYDRATION_WAIT`] (seconds), as for `base build`.
const HYDRATION_WAIT_ENV: &str = super::base::build::HYDRATION_TIMEOUT_ENV;

pub fn run(ctx: &Context, args: &ServeArgs) -> Result<u8, AppError> {
    let mut forwards = args.forward.clone();
    let ready_port = args
        .ready_port
        .or_else(|| forwards.iter().find(|f| f.listen == 0).map(|f| f.target));
    let mut reserved = Vec::new();
    // Reserve explicit ports first so the OS cannot give one to an auto forward.
    for auto in [false, true] {
        for forward in forwards.iter_mut().filter(|f| (f.listen == 0) == auto) {
            reserved.push(forward.reserve_local()?);
        }
    }
    forwards.extend(args.reverse.iter().cloned());
    let serve = Serve {
        reserved,
        forwards,
        ready_port,
        desktop: args.desktop,
        max_minutes: args.max_minutes,
    };
    super::run::run_as(ctx, &args.command_args(), Some(serve))
}

/// What makes a `run` a server.
pub(crate) struct Serve {
    pub reserved: Vec<std::net::TcpListener>,
    pub forwards: Vec<Forward>,
    pub ready_port: Option<u16>,
    pub desktop: bool,
    /// 0 disables the cap.
    pub max_minutes: f64,
}

impl Serve {
    /// The time cap, if any.
    pub fn cap(&self) -> Option<Duration> {
        (self.max_minutes > 0.0).then(|| Duration::from_secs_f64(self.max_minutes * 60.0))
    }

    /// The `ssh` options of the server's own connection.
    pub fn ssh_options(&self) -> Vec<String> {
        self.forwards.iter().flat_map(Forward::ssh_args).collect()
    }

    /// Before connecting: every local port is free here, and a forward that
    /// listens beyond the loopback address is said out loud.
    pub fn describe_bindings(&self, stream: &mut Stream) {
        for forward in &self.forwards {
            if !forward.is_loopback() {
                stream.note(format_args!(
                    "{forward}: listens on {}, which other machines may reach",
                    forward.bind
                ));
            }
        }
    }

    /// The line logged once the ready port listens.
    pub fn ready_message(&self, after: Duration) -> String {
        let Some(port) = self.ready_port else {
            return format!(
                "first desktop window appeared ({:.1}s after the server started)",
                after.as_secs_f64()
            );
        };
        let mut message = format!(
            "port {port} is listening on the sandbox ({:.1}s after the server started)",
            after.as_secs_f64()
        );
        let local: Vec<String> = self
            .forwards
            .iter()
            .filter(|forward| forward.direction == Direction::Local && forward.target == port)
            .map(Forward::url)
            .collect();
        if !local.is_empty() {
            message.push_str(&format!("; here at {}", local.join(", ")));
        }
        message
    }

    pub fn urls(&self) -> Vec<String> {
        self.forwards
            .iter()
            .filter(|f| f.direction == Direction::Local)
            .map(Forward::url)
            .collect()
    }

    pub fn environment(&self) -> Vec<String> {
        let urls = self.urls();
        let mut env = Vec::new();
        if let Some(first) = urls.first() {
            env.extend([
                "BOATPARTY_FORWARD_URL".to_string(),
                first.clone(),
                "BOATPARTY_FORWARD_URLS".to_string(),
                urls.join(" "),
            ]);
            for f in self
                .forwards
                .iter()
                .filter(|f| f.direction == Direction::Local)
            {
                env.extend([format!("BOATPARTY_FORWARD_{}_URL", f.target), f.url()]);
            }
        }
        env
    }

    /// The server's part of the result frame.
    pub fn json(&self) -> Value {
        let list = |direction: Direction| -> Vec<Value> {
            self.forwards
                .iter()
                .filter(|forward| forward.direction == direction)
                .map(|forward| {
                    json!({
                        "bind": forward.bind,
                        "listen": forward.listen,
                        "target": forward.target,
                    })
                })
                .collect()
        };
        json!({
            "forwards": list(Direction::Local),
            "reverses": list(Direction::Reverse),
            "desktop": self.desktop,
            "ready_port": self.ready_port,
            "max_minutes": self.max_minutes,
        })
    }
}

/// Wait until the member's files are fully restored before the server
/// starts: a server reads (and maps) many build files at once, which is
/// slow and fragile while they still arrive. Returns the exit status of a
/// SIGINT or SIGTERM that ended the wait.
pub(crate) fn wait_hydrated(
    ctx: &Context,
    stream: &mut Stream,
    member: &str,
    id: &str,
    first: Hydration,
    interrupts: &Interrupts,
) -> Result<Option<u8>, AppError> {
    if first.is_complete() {
        return Ok(None);
    }
    let provider = super::open_provider(ctx)?;
    let timeout = std::env::var(HYDRATION_WAIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map_or(HYDRATION_WAIT, Duration::from_secs);
    let deadline = Deadline::after(timeout);
    let started = Instant::now();
    stream.note(format_args!(
        "waiting for {member} to finish restoring its files ({}) before starting the server",
        describe(first)
    ));
    loop {
        if let Some(status) = interrupts.exit_status() {
            return Ok(Some(status));
        }
        if !deadline.tick() {
            stream.note(format_args!(
                "{member} is still restoring its files after {}s; starting the server anyway",
                timeout.as_secs()
            ));
            return Ok(None);
        }
        let sandbox = wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.get(id)
        })?;
        if sandbox.hydration.is_complete() {
            stream.note(format_args!(
                "{member} restored its files in {:.0}s",
                started.elapsed().as_secs_f64()
            ));
            return Ok(None);
        }
        if sandbox.state != SandboxState::Running {
            return Err(AppError::new(
                "not_running",
                format!(
                    "{member} ({id}) became {} while the server waited for its files",
                    sandbox.state
                ),
            ));
        }
    }
}

fn describe(hydration: Hydration) -> String {
    match hydration {
        Hydration::InProgress {
            percent: Some(percent),
        } => format!("{percent}% done"),
        Hydration::InProgress { percent: None } => "in progress".to_string(),
        Hydration::Unknown => "not reported yet".to_string(),
        Hydration::Complete => "complete".to_string(),
    }
}

/// SIGINT and SIGTERM, plus the time cap: once it passes, the server is
/// stopped the way a first SIGINT stops it.
pub(crate) struct CapProbe<'a> {
    interrupts: &'a Interrupts,
    deadline: Option<Instant>,
    minutes: f64,
    fired: Cell<bool>,
}

impl<'a> CapProbe<'a> {
    pub fn new(interrupts: &'a Interrupts, cap: Option<Duration>, minutes: f64) -> Self {
        Self {
            interrupts,
            deadline: cap.map(|cap| Instant::now() + cap),
            minutes,
            fired: Cell::new(false),
        }
    }

    /// The cap stopped the server.
    pub fn fired(&self) -> bool {
        self.fired.get()
    }
}

impl CancelProbe for CapProbe<'_> {
    fn state(&self) -> CancelState {
        let state = self.interrupts.state();
        if state != CancelState::None {
            return state;
        }
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => {
                if !self.fired.replace(true) {
                    eprintln!(
                        "boatparty: stopping the server after {} minutes (--max-minutes; 0 disables the cap)",
                        self.minutes
                    );
                }
                CancelState::Soft
            }
            _ => CancelState::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::forward::{parse_local, parse_reverse};

    fn serve(forwards: &[&str], ready_port: Option<u16>) -> Serve {
        Serve {
            reserved: Vec::new(),
            forwards: forwards
                .iter()
                .map(|spec| {
                    spec.strip_prefix("R")
                        .map_or_else(|| parse_local(spec), parse_reverse)
                        .unwrap()
                })
                .collect(),
            ready_port,
            desktop: false,
            max_minutes: 120.0,
        }
    }

    #[test]
    fn forwards_become_options_of_the_servers_own_connection() {
        let serve = serve(&["8000:8000", "R20000:7777"], Some(8000));
        assert_eq!(
            serve.ssh_options(),
            [
                "-L",
                "127.0.0.1:8000:127.0.0.1:8000",
                "-R",
                "127.0.0.1:20000:127.0.0.1:7777"
            ]
        );
        let message = serve.ready_message(Duration::from_millis(2500));
        assert!(message.starts_with("port 8000 is listening on the sandbox (2.5s"));
        assert!(
            message.ends_with("here at http://127.0.0.1:8000"),
            "{message}"
        );
        let value = serve.json();
        assert_eq!(value["forwards"][0]["bind"], "127.0.0.1");
        assert_eq!(value["reverses"][0]["listen"], 20000);
    }

    #[test]
    fn the_time_cap_defaults_on_and_zero_disables_it() {
        let mut serve = serve(&[], None);
        assert_eq!(serve.cap(), Some(Duration::from_secs(7200)));
        serve.max_minutes = 0.0;
        assert_eq!(serve.cap(), None);
        serve.max_minutes = 0.5;
        assert_eq!(serve.cap(), Some(Duration::from_secs(30)));
    }
}
