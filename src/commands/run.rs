//! `boatparty run`: sync, run one command in the sandbox's checkout,
//! stream its output, return its exit code and apply its file changes back.
//!
//! Warm path: when the workspace's member has a live SSH master, nothing
//! before the command starts talks to the provider. The lease is a local
//! state write; the heartbeat that keeps the member up starts in the
//! background when the runner reports the command started; the idle deadline
//! is set afterwards by a detached helper.
//!
//! Before launch permission, an SSH 255 may retry sync once. After permission,
//! a run reconnects to its nonce and resumes both streams by byte offset.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::WorkspaceContext;
use super::serve::{self, CapProbe, Serve};
use crate::Context;
use crate::cli::RunArgs;
use crate::error::AppError;
use crate::pool::lease::{self, Heartbeat, LeaseTimes};
use crate::pool::{LeaseRequest, warm_member};
use crate::project::Workspace;
use crate::provider::Hydration;
use crate::runner::{RunnerSpec, SshRunner};
use crate::runs;
use crate::signals::Interrupts;
use crate::state::{LeaseKind, Member, Store, SyncState};
use crate::stream::Stream;
use crate::sync::pack::usable_known;
use crate::sync::{
    self, History, MirrorRequest, PackRoute, PullBack, RunCredentials, RunEnd, RunOutcome,
    SeedConfig, SeedRoute, Sink, TokenSource, WorktreeInfo,
};
use crate::system;
use crate::transport::ssh::{SshTarget, shell_quote};

pub fn run(ctx: &Context, args: &RunArgs) -> Result<u8, AppError> {
    run_as(ctx, args, None)
}

/// `run`, or with `serve` a server (`serve`).
pub(crate) fn run_as(ctx: &Context, args: &RunArgs, serve: Option<Serve>) -> Result<u8, AppError> {
    let mut stream = Stream::new(ctx.out.is_json());
    let mut run = Run {
        ctx,
        serve,
        interrupted: None,
        // A named command's line is known once the project is loaded.
        line: command_line(args).unwrap_or_default(),
        named: args
            .words
            .split_first()
            .map(|(name, rest)| (name.clone(), rest.to_vec())),
        copy_back: true,
        began: Instant::now(),
        at: system::now_ms(),
        record: json!({}),
    };
    let status = match run.execute(&mut stream) {
        Ok(status) => status,
        Err(error) => {
            stream.error(&error);
            run.record["exit_code"] = Value::Null;
            run.record["error"] = json!(error.code());
            run.interrupted.unwrap_or_else(|| error.exit_code())
        }
    };
    run.record["at"] = json!(runs::rfc3339(run.at));
    if run.line.is_empty() {
        run.line = args.words.join(" ");
    }
    run.record["command"] = json!(run.line);
    run.record["kind"] = json!(run.kind().as_str());
    run.record["seconds"] = json!(round(run.began.elapsed().as_secs_f64()));
    if let Err(error) = runs::append(&ctx.paths, &run.record) {
        stream.note(format_args!("warning: could not record the run: {error}"));
    }
    Ok(status)
}

/// The shell line to run: `--sh` as given, or every word after `--`
/// quoted. `None` for the named form, which needs the project's
/// configuration.
pub fn command_line(args: &RunArgs) -> Option<String> {
    if let Some(line) = &args.sh {
        return Some(line.clone());
    }
    if !args.words.is_empty() {
        return None;
    }
    Some(
        args.command
            .iter()
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

struct Run<'a> {
    ctx: &'a Context,
    /// Set for a server (`serve`).
    serve: Option<Serve>,
    /// A SIGINT or SIGTERM ended the run before the command started.
    interrupted: Option<u8>,
    line: String,
    /// `run <name> [args...]`: the name and its arguments.
    named: Option<(String, Vec<String>)>,
    /// `[sync] copy_back`.
    copy_back: bool,
    began: Instant,
    at: u64,
    /// The runs.jsonl record, filled in as the run goes.
    record: Value,
}

/// The member serving this invocation and how it was reached.
struct Conn {
    queued: std::time::Duration,
    waited: std::time::Duration,
    recovery: Option<&'static str>,
    member: Member,
    target: SshTarget,
    /// Reached through a live master with no provider call.
    warm: bool,
    /// This invocation created or resumed the member.
    started_member: bool,
    /// How far the member's files are restored, when a provider call said.
    hydration: Option<Hydration>,
    restore_wait: Duration,
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
    reattached: bool,
    prep_seconds: Option<f64>,
    /// A setup or ensure step that failed, so the command did not run
    /// (`ensure.<name>`).
    failed_step: Option<String>,
    /// A server's time cap stopped it.
    capped: bool,
    /// Seconds from a server's start until its ready port listened.
    ready_seconds: Option<f64>,
}

impl Run<'_> {
    fn kind(&self) -> LeaseKind {
        if self.serve.is_some() {
            LeaseKind::Serve
        } else {
            LeaseKind::Run
        }
    }

    fn execute(&mut self, stream: &mut Stream) -> Result<u8, AppError> {
        let ctx = self.ctx;
        let workspace = Workspace::current()?;
        let project = workspace.load_project(&ctx.paths)?;
        if let Some((command, args)) = &self.named {
            self.record["name"] = json!(command);
            self.line = project.named_command(command, args)?;
        }
        self.copy_back = project.copy_back();
        let spec = RunnerSpec::with_env(
            project.config(),
            self.serve
                .as_ref()
                .map(Serve::environment)
                .unwrap_or_default(),
        );
        let name = project.name().to_string();
        let history = project.history();
        let mut seed = project.seed_config();
        let identity = project.git_identity();
        let token_source = project.run_token_source();
        // Local checks first: a repository without a commit fails before
        // anything remote happens.
        let info = WorktreeInfo::read(&workspace.root)?;
        let account = super::load_account(ctx)?;
        let times = LeaseTimes::for_project(&account.config, &name);
        let store = Store::new(&ctx.paths);
        self.record["project"] = json!(name);
        self.record["workspace"] = json!(workspace.key());
        if let Some(serve) = &self.serve {
            serve.describe_bindings(stream);
        }

        // A server always asks the provider: it must know whether the
        // member's files are restored, and it is not on the warm path.
        let mut conn = if self.serve.is_some() {
            bring_up(ctx, &self.line, LeaseKind::Serve)?
        } else {
            connect(ctx, &store, &name, &workspace, &self.line)?
        };
        if let Some(pin) = &conn.member.environment {
            let latest = project.environment().and_then(|expected| {
                store
                    .load()
                    .ok()?
                    .project(&name)?
                    .environment_versions
                    .get(expected)
                    .copied()
            });
            if let Some(warning) = pin.warning(project.environment(), latest) {
                stream.note(format_args!(
                    "{}: {warning}; see boatparty env apply",
                    conn.member.name
                ));
            }
        }
        // Read only now that a command is about to run; held in memory.
        let credentials = RunCredentials::gather(&workspace.root, identity, &token_source);
        if token_source != TokenSource::None && credentials.github_token.is_none() {
            stream.note(
                "[run] github_token is on but no GitHub token was found (set \
                 BOATPARTY_GITHUB_TOKEN or run `gh auth login`); the command runs without one",
            );
        }
        if let Some(token) = &credentials.github_token
            && seed.token.is_fetched()
        {
            // One `gh auth token` serves the seed too.
            seed.token = TokenSource::Fixed(token.clone());
        }
        self.record["git_identity"] = json!(credentials.has_identity());
        self.record["github_token"] = json!(credentials.github_token.is_some());
        let interrupts = Interrupts::install()?;
        let mut heartbeat = None;
        let ran = match self.prepare_serve(stream, &mut conn, times, &interrupts, &mut heartbeat) {
            Ok(()) => self.drive(
                stream,
                &name,
                &info,
                history,
                &seed,
                &spec,
                &credentials,
                &mut conn,
                times,
                &interrupts,
                &mut heartbeat,
            ),
            Err(error) => Err(error),
        };
        if let Err(e) = &ran
            && e.code() == "degraded_timeout"
            && let Err(err) = lease::mark_broken(&store, &name, &conn.id(), e.message())
        {
            stream.note(format_args!(
                "could not record the timeout for recovery: {err}"
            ));
        }
        // The token is not kept past the command.
        drop(credentials);
        drop(seed);
        self.record["member"] = json!(conn.member.name);
        self.record["sandbox"] = json!(conn.id());
        self.record["queued_seconds"] = json!(round(conn.queued.as_secs_f64()));
        self.record["waited_seconds"] = json!(round(conn.waited.as_secs_f64()));
        self.record["recovery"] = json!(conn.recovery);
        self.record["restore_wait_seconds"] = json!(round(conn.restore_wait.as_secs_f64()));
        self.record["warm"] = json!(conn.warm);
        self.record["started_member"] = json!(conn.started_member);

        let cancelled = interrupts.exit_status();
        let (status, copy_back) = match &ran {
            Ok(ran) => self.conclude(stream, &info, &name, &conn, ran, cancelled),
            Err(_) => (None, json!({"status": "none"})),
        };
        // Only now: bringing a large change back can outlast the deadline.
        if let Some(mut heartbeat) = heartbeat.take() {
            heartbeat.stop();
        }
        let id = conn.id();
        let last_run = json!({
            "at": runs::rfc3339(self.at),
            "kind": self.kind().as_str(),
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
        if self.serve.is_none() {
            self.record["reattached"] = json!(ran.reattached);
        }
        self.record["retried"] = json!(ran.retried);
        let steal = match ran.outcome.end {
            RunEnd::Done {
                cpu_steal_percent, ..
            } => cpu_steal_percent,
            RunEnd::Lost { .. } => None,
        };
        self.record["cpu_steal_percent"] = json!(steal);
        if let Some(percent @ 15..) = steal {
            stream.note(format!("boat host was overloaded ({percent}% CPU steal). Timing-sensitive failures may be caused by host contention."));
        }
        self.record["prep_seconds"] = json!(ran.prep_seconds);
        self.record["pack_bytes"] = json!(ran.outcome.pack_bytes);
        self.record["pack_route"] = json!(ran.outcome.pack_route.as_str());
        self.record["seed_route"] = json!(ran.outcome.seed.route.as_str());
        self.record["seed_fallback"] = json!(ran.outcome.seed.fallback);
        self.record["copy_back"] = copy_back["status"].clone();
        self.record["failed_step"] = json!(ran.failed_step);
        if self.serve.is_some() {
            self.record["capped"] = json!(ran.capped);
            self.record["ready_seconds"] = json!(ran.ready_seconds);
        }
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

    /// For a server, before it runs: keep the member up from now on, and
    /// wait until its files are restored. A SIGINT or SIGTERM during the
    /// wait ends the run.
    fn prepare_serve(
        &mut self,
        stream: &mut Stream,
        conn: &mut Conn,
        times: LeaseTimes,
        interrupts: &Interrupts,
        heartbeat: &mut Option<Heartbeat>,
    ) -> Result<(), AppError> {
        if self.serve.is_none() {
            return Ok(());
        }
        let id = conn.id();
        *heartbeat = Some(Heartbeat::for_member(&self.ctx.paths, &id, times));
        let hydration = conn.hydration.unwrap_or(Hydration::Unknown);
        let wait_started = Instant::now();
        let interrupted = serve::wait_hydrated(
            self.ctx,
            stream,
            &conn.member.name,
            &id,
            hydration,
            interrupts,
        )?;
        if !hydration.is_complete() {
            conn.restore_wait += wait_started.elapsed();
        }
        if let Some(status) = interrupted {
            self.interrupted = Some(status);
            return Err(AppError::new(
                "interrupted",
                "cancelled while waiting for the sandbox's files; the server did not start",
            ));
        }
        Ok(())
    }

    /// Run the command, retrying once under the 255 rule.
    #[allow(clippy::too_many_arguments)]
    fn drive(
        &mut self,
        stream: &mut Stream,
        name: &str,
        info: &WorktreeInfo,
        history: History,
        seed: &SeedConfig,
        spec: &RunnerSpec,
        credentials: &RunCredentials,
        conn: &mut Conn,
        times: LeaseTimes,
        interrupts: &Interrupts,
        heartbeat: &mut Option<Heartbeat>,
    ) -> Result<Ran, AppError> {
        let mut retried = false;
        let probe = CapProbe::new(
            interrupts,
            self.serve.as_ref().and_then(Serve::cap),
            self.serve.as_ref().map_or(0.0, |serve| serve.max_minutes),
        );
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
                seed: Some(seed),
            };
            let mut host = super::runner_host(self.ctx, &conn.target, &conn.member, spec.clone());
            if let Some(serve) = &mut self.serve {
                serve.reserved.clear();
                // The server's invocation, and its forwards, on a connection
                // of its own.
                host.dedicated = Some(serve.ssh_options());
            }
            let mut sink = StartWatch {
                stream: &mut *stream,
                paths: &self.ctx.paths,
                id: conn.id(),
                times,
                heartbeat: &mut *heartbeat,
                started: None,
                failed_step: None,
                serve: self.serve.as_ref(),
                ready_seconds: None,
                reattached: false,
                mux_refused: false,
            };
            let outcome = match &self.serve {
                Some(serve) => sync::serve_command_on(
                    &host,
                    &request,
                    &self.line,
                    credentials,
                    sync::ServeOptions {
                        ready_port: serve.ready_port,
                        desktop: serve.desktop,
                    },
                    &mut sink,
                    &probe,
                )?,
                None => sync::mirror::detached_command(
                    &host,
                    &request,
                    &self.line,
                    credentials,
                    &mut sink,
                    &probe,
                    &|| {
                        if matches!(conn.target.run_quiet("true"), Ok((0, _))) {
                            return Ok(true);
                        }
                        conn.target.repair_master();
                        if matches!(conn.target.run_quiet("true"), Ok((0, _))) {
                            return Ok(true);
                        }
                        // The existing lease remains with this process through retries.
                        let restored = WorkspaceContext::open(self.ctx).and_then(|wctx| {
                            wctx.pool(self.ctx)
                                .bring_up(wctx.bring_up_options(true), None)
                        });
                        let Ok(restored) = restored else {
                            return Ok(false);
                        };
                        if restored.member.id != conn.member.id {
                            return Err(AppError::new(
                                "run_failed",
                                "command lost: the sandbox was replaced",
                            ));
                        }
                        Ok(true)
                    },
                )?,
            };
            let started = sink.started;
            let prep_seconds = started.map(|at| round(at.duration_since(self.began).as_secs_f64()));
            let failed_step = sink.failed_step.take();
            let ready_seconds = sink.ready_seconds;
            let reattached = sink.reattached;
            let mux_refused = sink.mux_refused;
            let ran = |outcome| Ran {
                outcome,
                digest: digest.clone(),
                retried,
                prep_seconds,
                reattached,
                failed_step: failed_step.clone(),
                capped: probe.fired(),
                ready_seconds,
            };
            match outcome.end {
                RunEnd::Done { .. } => return Ok(ran(outcome)),
                RunEnd::Lost { .. } if outcome.cancelled => return Ok(ran(outcome)),
                // A server is started again only if it never started.
                RunEnd::Lost { status: Some(255) }
                    if !retried
                        && (!conn.started_member || mux_refused)
                        && interrupts.exit_status().is_none()
                        && (self.serve.is_none() || started.is_none()) =>
                {
                    if mux_refused {
                        conn.target.repair_master();
                        if matches!(conn.target.run_quiet("true"), Ok((0, _))) {
                            stream.note(
                                "replaced a stale SSH control master before the command started",
                            );
                            retried = true;
                            continue;
                        }
                    }
                    stream.note(format_args!(
                        "the connection to {} ended without a result (ssh exit 255); reconnecting and running the command again",
                        conn.member.name
                    ));
                    if let Some(mut beat) = heartbeat.take() {
                        beat.stop();
                    }
                    conn.target.close_master();
                    let warm = conn.warm;
                    let mut next = bring_up(self.ctx, &self.line, self.kind())?;
                    next.restore_wait += conn.restore_wait;
                    next.queued += conn.queued;
                    next.waited += conn.waited;
                    next.recovery = next.recovery.or(conn.recovery);
                    *conn = next;
                    conn.warm = warm;
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
        if !self.copy_back {
            stream.note("the command changed files in the checkout; [sync] copy_back = false leaves them there");
            return (Some(exit_code), json!({"status": "off"}));
        }
        // A large patch comes back by signed download; only then is the
        // provider opened.
        let threshold = crate::runner::upload_threshold();
        let upload = (patch_bytes > threshold)
            .then(|| {
                let provider = super::open_provider(self.ctx).ok()?;
                Some(crate::runner::SignedUpload {
                    provider,
                    sandbox_id: conn.member.id.clone()?,
                    threshold,
                })
            })
            .flatten();
        let host = SshRunner {
            upload,
            ..SshRunner::ssh_only(conn.target.clone())
        };
        let mut route = PackRoute::Ssh;
        let applied =
            sync::fetch_patch_routed(&host, name, &ran.outcome.nonce, patch_bytes).and_then(
                |fetched| {
                    route = fetched.route;
                    if let Some(reason) = &fetched.fallback {
                        stream.note(format_args!(
                            "the signed download of the command's changes failed ({reason}); fetched them over SSH instead"
                        ));
                    }
                    sync::apply_pullback(info, &fetched.patch)
                },
            );
        let summary = match applied {
            Ok(PullBack::Applied { files }) => {
                stream.note(format_args!(
                    "applied the command's changes locally: {}",
                    files.join(", ")
                ));
                json!({"status": "applied", "files": files, "patch": null, "route": route.as_str()})
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
                json!({"status": "conflict", "files": files, "patch": kept.display().to_string(), "route": route.as_str()})
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
            let mut result = json!({
                "ok": true,
                "kind": self.kind().as_str(),
                "exit_code": exit,
                "command_exit_code": status,
                "cancelled": cancelled,
                "project": self.record["project"],
                "workspace": self.record["workspace"],
                "member": { "name": conn.member.name, "id": conn.id() },
                "command": self.line,
                "seconds": seconds,
                "cpu_steal_percent": self.record["cpu_steal_percent"],
                "prep_seconds": ran.prep_seconds,
                "queued_seconds": self.record["queued_seconds"],
                "waited_seconds": self.record["waited_seconds"],
                "recovery": conn.recovery,
                "warm": conn.warm,
                "restore_wait_seconds": self.record["restore_wait_seconds"],
                "started_member": conn.started_member,
                "retried": ran.retried,
                "pack_bytes": ran.outcome.pack_bytes,
                "pack_route": ran.outcome.pack_route.as_str(),
                "upload_fallback": ran.outcome.upload_fallback,
                "seed_route": ran.outcome.seed.route.as_str(),
                "seed_fallback": ran.outcome.seed.fallback,
                "seed_skipped": ran.outcome.seed.skipped,
                "seed_seconds": ran.outcome.seed.seconds(),
                "full_retry": ran.outcome.full_retry,
                "runner_installed": ran.outcome.installed,
                "failed_step": ran.failed_step,
                "git_identity": self.record["git_identity"],
                "github_token": self.record["github_token"],
                "copy_back": copy_back,
            });
            if let Some(serve) = &self.serve {
                if let (Value::Object(map), Value::Object(extra)) = (&mut result, serve.json()) {
                    map.extend(extra);
                }
                result["capped"] = json!(ran.capped);
                result["ready_seconds"] = json!(ran.ready_seconds);
            }
            if self.serve.is_none() {
                result["reattached"] = json!(ran.reattached);
            }
            stream.result(result);
            return;
        }
        let mut notes = Vec::new();
        if !conn.queued.is_zero() {
            notes.push(format!(
                "queued {:.1}s for a start",
                conn.queued.as_secs_f64()
            ));
        }
        if !conn.waited.is_zero() {
            notes.push(format!(
                "waited {:.1}s for another process to start the member",
                conn.waited.as_secs_f64()
            ));
        }
        if let Some(action) = conn.recovery {
            notes.push(format!("{action} the sandbox"));
        }

        if let Some(prep) = ran.prep_seconds {
            notes.push(format!("prep {prep:.1}s"));
        }
        if let Some(percent) = self.record["cpu_steal_percent"].as_u64() {
            notes.push(format!("{percent}% CPU steal"));
        }
        if conn.started_member {
            notes.push("started the sandbox".to_string());
        }
        if ran.retried {
            notes.push("ran again after a lost connection".to_string());
        }
        if ran.outcome.seed.route == SeedRoute::Github {
            notes.push(format!(
                "seeded from GitHub in {:.1}s",
                ran.outcome.seed.seconds()
            ));
        }
        if ran.outcome.pack_route == PackRoute::SignedUrl {
            notes.push("pack sent by signed upload".to_string());
        }
        if let Some(reason) = &ran.outcome.upload_fallback {
            notes.push(format!(
                "signed upload failed ({reason}), pack sent over SSH"
            ));
        }
        if let Some(step) = &ran.failed_step {
            notes.push(format!(
                "the {step} step failed, so the command did not run"
            ));
        }
        if ran.capped {
            notes.push("stopped by the time cap".to_string());
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
    reattached: bool,
    mux_refused: bool,
    stream: &'a mut Stream,
    paths: &'a crate::paths::Paths,
    id: String,
    times: LeaseTimes,
    heartbeat: &'a mut Option<Heartbeat>,
    started: Option<Instant>,
    failed_step: Option<String>,
    /// A server's settings, for its ready line.
    serve: Option<&'a Serve>,
    ready_seconds: Option<f64>,
}

impl Sink for StartWatch<'_> {
    fn stdout(&mut self, bytes: &[u8]) {
        self.stream.stdout(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        self.stream.stderr(bytes);
    }

    fn note(&mut self, message: &str) {
        self.stream.note(message);
    }

    fn event(&mut self, name: &str) {
        if name == "mux-refused" {
            self.mux_refused = true;
            return;
        }
        if name == "reattached" {
            self.reattached = true;
        }
        // Steps (dependency installs) can take longer than the auto-stop
        // deadline, so the heartbeat starts with them.
        if (name == "started" || name == "preparing") && self.heartbeat.is_none() {
            *self.heartbeat = Some(Heartbeat::for_member(self.paths, &self.id, self.times));
        }
        if name == "started" && self.started.is_none() {
            self.started = Some(Instant::now());
        }
        if let Some(step) = name.strip_prefix("failed-step ") {
            self.failed_step = Some(step.to_string());
        }
        if name == "ready"
            && self.ready_seconds.is_none()
            && let Some(serve) = self.serve
        {
            let after = self.started.map(|at| at.elapsed()).unwrap_or_default();
            self.ready_seconds = Some(round(after.as_secs_f64()));
            self.stream.note(serve.ready_message(after));
            self.stream.ready(serve.urls());
            return;
        }
        self.stream.event(name);
    }
}

/// This workspace's member with this process's `run` lease on it: through a
/// live master with no provider call when the member is still this
/// workspace's, else by acquiring and bringing it up.
fn connect(
    ctx: &Context,
    store: &Store,
    name: &str,
    workspace: &Workspace,
    line: &str,
) -> Result<Conn, AppError> {
    let key = workspace.key();
    if let Some((member, target)) = warm_member(&ctx.paths, name, &key)?
        && lease::register(store, name, &key, &member, LeaseKind::Run, line)?
    {
        return Ok(Conn {
            member,
            target,
            queued: Default::default(),
            waited: Default::default(),
            recovery: None,
            warm: true,
            started_member: false,
            hydration: None,
            restore_wait: Duration::ZERO,
        });
    }
    bring_up(ctx, line, LeaseKind::Run)
}

fn bring_up(ctx: &Context, line: &str, kind: LeaseKind) -> Result<Conn, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    if let Err(e) = super::env::expected_version(&wctx, ctx) {
        ctx.out.note(format_args!(
            "cannot verify latest environment version: {e}"
        ));
    }
    let up = pool.bring_up(
        wctx.bring_up_options(false),
        Some(&LeaseRequest::new(kind, line)),
    )?;
    Ok(Conn {
        queued: pool.queued.get(),
        waited: pool.waited.get(),
        recovery: pool.recovery.get(),
        member: up.member,
        target: up.target,
        warm: false,
        started_member: up.created || up.resumed,
        hydration: Some(up.sandbox.hydration),
        restore_wait: up.restore_wait,
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
        let argv = RunArgs {
            sh: None,
            words: Vec::new(),
            command: vec!["sh".into(), "-c".into(), "echo out; exit 7".into()],
        };
        assert_eq!(
            command_line(&argv).as_deref(),
            Some("sh -c 'echo out; exit 7'")
        );
        let line = RunArgs {
            sh: Some("cargo test | tail -n 3".into()),
            words: Vec::new(),
            command: Vec::new(),
        };
        assert_eq!(
            command_line(&line).as_deref(),
            Some("cargo test | tail -n 3")
        );
        let named = RunArgs {
            sh: None,
            words: vec!["test".into(), "-q".into()],
            command: Vec::new(),
        };
        assert_eq!(command_line(&named), None);
    }
}
