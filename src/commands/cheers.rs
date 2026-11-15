//! `boatparty cheers`: sync, run one command in the sandbox's checkout,
//! stream its output, return its exit code and apply its file changes back.
//!
//! Warm path: when the workspace's member has a live SSH master, nothing
//! before the command starts talks to the provider. The lease is a local
//! state write; the heartbeat that keeps the member up starts in the
//! background when the runner reports the command started; the idle deadline
//! is set afterwards by a detached helper.
//!
//! Retry rule: only an SSH exit of 255 without a trailer, and only when this
//! invocation did not itself start the member, is run again once from the
//! top (the usual cause is a member that auto-stopped under a lingering
//! master). Nothing else is ever re-run.

use std::time::Instant;

use serde_json::{Value, json};

use super::WorkspaceContext;
use crate::Context;
use crate::cli::CheersArgs;
use crate::error::AppError;
use crate::pool::lease::{self, Heartbeat, LeaseTimes};
use crate::pool::warm_member;
use crate::project::Workspace;
use crate::runner::SshRunner;
use crate::runs;
use crate::signals::Interrupts;
use crate::state::{LeaseKind, Member, Store, SyncState};
use crate::stream::Stream;
use crate::sync::pack::usable_known;
use crate::sync::{
    self, History, MirrorRequest, PackRoute, PullBack, RunEnd, RunOutcome, Sink, WorktreeInfo,
};
use crate::system;
use crate::transport::ssh::{SshTarget, shell_quote};

pub fn run(ctx: &Context, args: &CheersArgs) -> Result<u8, AppError> {
    let line = command_line(args);
    let mut stream = Stream::new(ctx.out.is_json());
    let mut cheers = Cheers {
        ctx,
        line,
        began: Instant::now(),
        at: system::now_ms(),
        record: json!({}),
    };
    let status = match cheers.execute(&mut stream) {
        Ok(status) => status,
        Err(error) => {
            stream.error(&error);
            cheers.record["exit_code"] = Value::Null;
            cheers.record["error"] = json!(error.code());
            error.exit_code()
        }
    };
    cheers.record["at"] = json!(runs::rfc3339(cheers.at));
    cheers.record["command"] = json!(cheers.line);
    cheers.record["kind"] = json!("run");
    cheers.record["seconds"] = json!(round(cheers.began.elapsed().as_secs_f64()));
    if let Err(error) = runs::append(&ctx.paths, &cheers.record) {
        stream.note(format_args!("warning: could not record the run: {error}"));
    }
    Ok(status)
}

/// The shell line to run: `--sh` as given, or every word quoted.
pub fn command_line(args: &CheersArgs) -> String {
    match &args.sh {
        Some(line) => line.clone(),
        None => args
            .command
            .iter()
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

struct Cheers<'a> {
    ctx: &'a Context,
    line: String,
    began: Instant,
    at: u64,
    /// The runs.jsonl record, filled in as the run goes.
    record: Value,
}

/// The member serving this invocation and how it was reached.
struct Conn {
    member: Member,
    target: SshTarget,
    /// Reached through a live master with no provider call.
    warm: bool,
    /// This invocation created or resumed the member.
    started_member: bool,
}

impl Conn {
    fn id(&self) -> String {
        self.member.id.clone().unwrap_or_default()
    }
}

/// What the run produced, for the release and the report.
struct Ran {
    outcome: RunOutcome,
    digest: String,
    retried: bool,
    prep_seconds: Option<f64>,
}

impl Cheers<'_> {
    fn execute(&mut self, stream: &mut Stream) -> Result<u8, AppError> {
        let ctx = self.ctx;
        let workspace = Workspace::current()?;
        let project = workspace.load_project()?;
        let name = project.name().to_string();
        let history = project.history();
        // Local checks first: a repository without a commit fails before
        // anything remote happens.
        let info = WorktreeInfo::read(&workspace.root)?;
        let account = super::load_account(ctx)?;
        let times = LeaseTimes::from_defaults(&account.config.defaults);
        let store = Store::new(&ctx.paths);
        self.record["project"] = json!(name);
        self.record["workspace"] = json!(workspace.key());

        let mut conn = connect(ctx, &name, &workspace)?;
        lease::register(&store, &name, &conn.member, LeaseKind::Run, &self.line)?;
        let interrupts = Interrupts::install()?;
        let mut heartbeat = None;
        let ran = self.drive(
            stream,
            &name,
            &info,
            history,
            &mut conn,
            &store,
            times,
            &interrupts,
            &mut heartbeat,
        );
        if let Some(mut heartbeat) = heartbeat.take() {
            heartbeat.stop();
        }
        self.record["member"] = json!(conn.member.name);
        self.record["sandbox"] = json!(conn.id());
        self.record["warm"] = json!(conn.warm);
        self.record["started_member"] = json!(conn.started_member);

        let cancelled = interrupts.exit_status();
        let (status, copy_back) = match &ran {
            Ok(ran) => self.conclude(stream, &info, &name, &conn, ran, cancelled),
            Err(_) => (None, json!({"status": "none"})),
        };
        let id = conn.id();
        let last_run = json!({
            "at": runs::rfc3339(self.at),
            "kind": "run",
            "command": self.line,
            "exit_code": status,
            "seconds": round(self.began.elapsed().as_secs_f64()),
        });
        let released = lease::finish(
            &store,
            &name,
            &id,
            |member| {
                member.last_used_at = system::now_ms();
                member.last_run = Some(last_run);
                if let Ok(ran) = &ran
                    && !ran.outcome.known.is_empty()
                {
                    member.sync = SyncState {
                        mode: history.as_str().to_string(),
                        known: ran.outcome.known.clone(),
                        refs_digest: Some(ran.digest.clone()),
                    };
                }
            },
            || {
                if let Err(error) = lease::spawn_idle_helper(&name, &id) {
                    stream.note(format_args!("warning: {error}"));
                }
            },
        );
        if let Err(error) = released {
            stream.note(format_args!(
                "warning: could not release the lease: {error}"
            ));
        }
        let ran = ran?;
        let exit = cancelled.unwrap_or_else(|| exit_byte(status.unwrap_or(125)));
        self.record["exit_code"] = json!(status);
        self.record["cancelled"] = json!(cancelled.is_some());
        self.record["retried"] = json!(ran.retried);
        self.record["prep_seconds"] = json!(ran.prep_seconds);
        self.record["pack_bytes"] = json!(ran.outcome.pack_bytes);
        self.record["pack_route"] = json!(ran.outcome.pack_route.as_str());
        self.record["copy_back"] = copy_back["status"].clone();
        self.report(
            stream,
            &conn,
            &ran,
            status,
            exit,
            cancelled.is_some(),
            copy_back,
        );
        Ok(exit)
    }

    /// Run the command, retrying once under the 255 rule.
    #[allow(clippy::too_many_arguments)]
    fn drive(
        &self,
        stream: &mut Stream,
        name: &str,
        info: &WorktreeInfo,
        history: History,
        conn: &mut Conn,
        store: &Store,
        times: LeaseTimes,
        interrupts: &Interrupts,
        heartbeat: &mut Option<Heartbeat>,
    ) -> Result<Ran, AppError> {
        let mut retried = false;
        loop {
            let snapshot = sync::snapshot(info)?;
            let (refs, digest) = sync::mirrored_refs(info, history)?;
            let sync_state = &conn.member.sync;
            let request = MirrorRequest {
                project: name,
                info,
                snapshot: &snapshot,
                refs: &refs,
                send_refs: sync_state.refs_digest.as_deref() != Some(digest.as_str()),
                known: usable_known(history, &sync_state.mode, &sync_state.known),
                history,
            };
            let host = super::runner_host(self.ctx, &conn.target, &conn.member);
            let mut sink = StartWatch {
                stream: &mut *stream,
                paths: &self.ctx.paths,
                id: conn.id(),
                times,
                heartbeat: &mut *heartbeat,
                started: None,
            };
            let outcome = sync::run_command(&host, &request, &self.line, &mut sink, interrupts)?;
            let prep_seconds = sink
                .started
                .map(|at| round(at.duration_since(self.began).as_secs_f64()));
            match outcome.end {
                RunEnd::Done { .. } => {
                    return Ok(Ran {
                        outcome,
                        digest,
                        retried,
                        prep_seconds,
                    });
                }
                RunEnd::Lost { .. } if outcome.cancelled => {
                    return Ok(Ran {
                        outcome,
                        digest,
                        retried,
                        prep_seconds,
                    });
                }
                RunEnd::Lost { status: Some(255) }
                    if !retried && !conn.started_member && interrupts.exit_status().is_none() =>
                {
                    stream.note(format_args!(
                        "the connection to {} ended without a result (ssh exit 255); reconnecting and running the command again",
                        conn.member.name
                    ));
                    if let Some(mut beat) = heartbeat.take() {
                        beat.stop();
                    }
                    conn.target.close_master();
                    let warm = conn.warm;
                    *conn = bring_up(self.ctx)?;
                    conn.warm = warm;
                    lease::register(store, name, &conn.member, LeaseKind::Run, &self.line)?;
                    retried = true;
                }
                RunEnd::Lost { status } => return Err(lost(status, &conn.member.name)),
            }
        }
    }

    /// Bring the command's file changes back. Returns the command's exit code
    /// (if it reported one) and the copy-back summary.
    fn conclude(
        &self,
        stream: &mut Stream,
        info: &WorktreeInfo,
        name: &str,
        conn: &Conn,
        ran: &Ran,
        cancelled: Option<u8>,
    ) -> (Option<i32>, Value) {
        let RunEnd::Done {
            exit_code,
            patch_bytes,
            ..
        } = ran.outcome.end
        else {
            return (None, json!({"status": "none"}));
        };
        if cancelled.is_some() || patch_bytes == 0 {
            return (Some(exit_code), json!({"status": "none"}));
        }
        let host = SshRunner::ssh_only(conn.target.clone());
        let applied = sync::fetch_patch(&host, name, &ran.outcome.nonce)
            .and_then(|patch| sync::apply_pullback(info, &patch));
        let summary = match applied {
            Ok(PullBack::Applied { files }) => {
                stream.note(format_args!(
                    "applied the command's changes locally: {}",
                    files.join(", ")
                ));
                json!({"status": "applied", "files": files, "patch": null})
            }
            Ok(PullBack::Conflict {
                files,
                kept,
                reason,
            }) => {
                stream.note(format_args!(
                    "CONFLICT: the command changed {} but the local copy changed too; nothing was changed locally and the patch is kept at {} ({reason})",
                    files.join(", "),
                    kept.display()
                ));
                json!({"status": "conflict", "files": files, "patch": kept.display().to_string()})
            }
            Err(error) => {
                stream.note(format_args!(
                    "warning: could not bring the command's changes back: {error}"
                ));
                json!({"status": "failed", "files": [], "patch": null, "error": error.message()})
            }
        };
        (Some(exit_code), summary)
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &self,
        stream: &mut Stream,
        conn: &Conn,
        ran: &Ran,
        status: Option<i32>,
        exit: u8,
        cancelled: bool,
        copy_back: Value,
    ) {
        let seconds = round(self.began.elapsed().as_secs_f64());
        if stream.is_json() {
            stream.result(json!({
                "ok": true,
                "exit_code": exit,
                "command_exit_code": status,
                "cancelled": cancelled,
                "project": self.record["project"],
                "workspace": self.record["workspace"],
                "member": { "name": conn.member.name, "id": conn.id() },
                "command": self.line,
                "seconds": seconds,
                "prep_seconds": ran.prep_seconds,
                "warm": conn.warm,
                "started_member": conn.started_member,
                "retried": ran.retried,
                "pack_bytes": ran.outcome.pack_bytes,
                "pack_route": ran.outcome.pack_route.as_str(),
                "upload_fallback": ran.outcome.upload_fallback,
                "full_retry": ran.outcome.full_retry,
                "runner_installed": ran.outcome.installed,
                "copy_back": copy_back,
            }));
            return;
        }
        let mut notes = Vec::new();
        if let Some(prep) = ran.prep_seconds {
            notes.push(format!("prep {prep:.1}s"));
        }
        if conn.started_member {
            notes.push("started the sandbox".to_string());
        }
        if ran.retried {
            notes.push("ran again after a lost connection".to_string());
        }
        if ran.outcome.pack_route == PackRoute::SignedUrl {
            notes.push("pack sent by signed upload".to_string());
        }
        if let Some(reason) = &ran.outcome.upload_fallback {
            notes.push(format!(
                "signed upload failed ({reason}), pack sent over SSH"
            ));
        }
        let what = match status {
            Some(code) if !cancelled => format!("exit {code}"),
            Some(code) => format!("cancelled (command exit {code})"),
            None => "cancelled".to_string(),
        };
        stream.note(format_args!(
            "{} ({}) {what} in {seconds:.1}s ({})",
            conn.member.name,
            conn.id(),
            notes.join(", ")
        ));
    }
}

/// Starts the heartbeat when the runner says the command started, and
/// remembers when that was.
struct StartWatch<'a> {
    stream: &'a mut Stream,
    paths: &'a crate::paths::Paths,
    id: String,
    times: LeaseTimes,
    heartbeat: &'a mut Option<Heartbeat>,
    started: Option<Instant>,
}

impl Sink for StartWatch<'_> {
    fn stdout(&mut self, bytes: &[u8]) {
        self.stream.stdout(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        self.stream.stderr(bytes);
    }

    fn event(&mut self, name: &str) {
        if name == "started" && self.started.is_none() {
            self.started = Some(Instant::now());
            if self.heartbeat.is_none() {
                *self.heartbeat = Some(Heartbeat::for_member(self.paths, &self.id, self.times));
            }
        }
        self.stream.event(name);
    }
}

fn connect(ctx: &Context, name: &str, workspace: &Workspace) -> Result<Conn, AppError> {
    if let Some((member, target)) = warm_member(&ctx.paths, name, &workspace.key())? {
        return Ok(Conn {
            member,
            target,
            warm: true,
            started_member: false,
        });
    }
    bring_up(ctx)
}

fn bring_up(ctx: &Context) -> Result<Conn, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let up = wctx.pool(ctx).bring_up(wctx.bring_up_options(false))?;
    Ok(Conn {
        member: up.member,
        target: up.target,
        warm: false,
        started_member: up.created || up.resumed,
    })
}

fn lost(status: Option<i32>, member: &str) -> AppError {
    match status {
        Some(255) => AppError::new(
            "connection",
            format!(
                "the session on {member} ended without a result (ssh exit 255); the sandbox may have stopped"
            ),
        ),
        Some(code) => AppError::new(
            "run_failed",
            format!("the runner on {member} ended without a result (exit {code})"),
        ),
        None => AppError::new(
            "run_failed",
            format!("the connection to {member} was cut before a result arrived"),
        ),
    }
}

fn exit_byte(code: i32) -> u8 {
    u8::try_from(code.clamp(0, 255)).unwrap_or(255)
}

fn round(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_words_are_quoted_and_a_shell_line_is_passed_as_is() {
        let argv = CheersArgs {
            sh: None,
            command: vec!["sh".into(), "-c".into(), "echo out; exit 7".into()],
        };
        assert_eq!(command_line(&argv), "sh -c 'echo out; exit 7'");
        let line = CheersArgs {
            sh: Some("cargo test | tail -n 3".into()),
            command: Vec::new(),
        };
        assert_eq!(command_line(&line), "cargo test | tail -n 3");
    }
}
