//! `boatparty base build`: the build sequence.
//!
//! 1. Bring the builder up (its starts pass the start ledger) and hold it
//!    with a lease; a heartbeat keeps its auto-stop ahead while it works.
//! 2. Sync the base ref in the project's history mode.
//! 3. Run the setup and ensure steps and `[base] warm` (the runner's `run`).
//! 4. Settle the checkout (the runner's `settle`).
//! 5. Wait until the provider reports the builder's files complete. A build
//!    whose hydration never completes fails here without stopping it.
//! 6. Stop the builder and wait until it is stopped.
//! 7. Save it as `bp-<project>`; boat replaces an existing name only once
//!    the new save is ready, and a failed save keeps the last good one.
//! 8. Wait until the save is ready.
//! 9. Record the base. Nothing before this step touches the record, so any
//!    failure leaves the previous base in use.

use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Context;
use crate::base::reference::{self, BaseRef};
use crate::base::{self, BaseRecord};
use crate::commands::WorkspaceContext;
use crate::error::AppError;
use crate::pool::lease::{self, Heartbeat, LeaseTimes};
use crate::pool::wait::{self, Deadline};
use crate::pool::{Pool, Up};
use crate::provider::{BusyReason, ErrorKind, Provider, Sandbox, SandboxState, SnapshotStatus};
use crate::runner::{EXIT_MISSING, RunnerHost, RunnerSpec, SshRunner};
use crate::signals::Interrupts;
use crate::state::{Member, SyncState};
use crate::sync::pack::usable_known;
use crate::sync::{
    self, MirrorRequest, RunCredentials, RunEnd, Sink, Snapshot, TokenSource, WorktreeInfo,
};
use crate::system;

/// Overrides how long a build waits for the builder's files to be complete
/// before it gives up without stopping it (seconds).
pub const HYDRATION_TIMEOUT_ENV: &str = "BOATPARTY_HYDRATION_TIMEOUT_SECONDS";
const HYDRATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STOP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SAVE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Polls to leave between settling and the stop: boat's stop snapshot has
/// been seen to miss writes made in its last moments.
const SETTLE_GAP_POLLS: u32 = 5;

/// What a build produced.
pub(crate) struct Built {
    pub record: BaseRecord,
    pub builder: Member,
    pub up: (bool, bool),
    pub base_ref: BaseRef,
    pub pack_bytes: u64,
    pub seed_route: &'static str,
    pub phases: Vec<(&'static str, f64)>,
}

/// How long each step took.
struct Phases {
    done: Vec<(&'static str, f64)>,
    since: Instant,
}

impl Phases {
    fn start() -> Self {
        Self {
            done: Vec::new(),
            since: Instant::now(),
        }
    }

    fn lap(&mut self, what: &'static str) {
        self.done
            .push((what, round(self.since.elapsed().as_secs_f64())));
        self.since = Instant::now();
    }
}

/// Where the build is, so a failure knows whether the builder may still
/// be running.
#[derive(PartialEq, Eq)]
enum Stage {
    Running,
    StopRequested,
}

pub(crate) fn build(
    ctx: &Context,
    wctx: &WorkspaceContext,
    interrupts: &Interrupts,
) -> Result<Built, AppError> {
    let project = &wctx.project;
    let name = project.name().to_string();
    let info = WorktreeInfo::read(&wctx.workspace.root)?;
    let base_ref = reference::resolve(&info, project.config().base.git_ref.as_deref())?;
    let history = project.history();
    let pool = wctx.pool(ctx);
    let times = LeaseTimes::from_settings(&wctx.settings);
    let mut phases = Phases::start();

    ctx.out.note(format_args!(
        "building the base of {name} from {} ({})",
        base::staleness::short_ref(&base_ref.name),
        short(&base_ref.commit)
    ));
    let up = crate::pool::bring_up_builder(
        &pool,
        crate::pool::BringUp {
            size: wctx.settings.machine,
            ttl_seconds: times.lease_seconds,
            extend_running: true,
        },
    )?;
    phases.lap("bring_up");
    let id = up.sandbox.id.clone();
    let mut heartbeat = Some(Heartbeat::for_member(&ctx.paths, &id, times));
    let mut stage = Stage::Running;
    let result = sequence(
        ctx,
        wctx,
        &pool,
        &up,
        &info,
        &base_ref,
        history,
        interrupts,
        &mut heartbeat,
        &mut stage,
        &mut phases,
    );
    if let Some(mut beat) = heartbeat.take() {
        beat.stop();
    }
    crate::pool::release_builder(&pool);
    match result {
        Ok((record, builder, pack_bytes, seed_route)) => Ok(Built {
            record,
            builder,
            up: (up.created, up.resumed),
            base_ref,
            pack_bytes,
            seed_route,
            phases: phases.done,
        }),
        Err(error) => {
            if stage == Stage::Running {
                // The builder stays up; the idle helper lets it stop by
                // itself, never inside a hydration it knows about.
                if let Err(spawn) = lease::spawn_idle_helper(&name, &id) {
                    ctx.out.note(format_args!("warning: {spawn}"));
                }
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sequence(
    ctx: &Context,
    wctx: &WorkspaceContext,
    pool: &Pool<'_>,
    up: &Up,
    info: &WorktreeInfo,
    base_ref: &BaseRef,
    history: sync::History,
    interrupts: &Interrupts,
    heartbeat: &mut Option<Heartbeat>,
    stage: &mut Stage,
    phases: &mut Phases,
) -> Result<(BaseRecord, Member, u64, &'static str), AppError> {
    let project = &wctx.project;
    let name = project.name();
    let provider = wctx.provider.as_ref();
    let id = up.sandbox.id.clone();
    let spec = RunnerSpec::new(project.config());

    // 2 and 3: the base ref as if it were checked out, then the steps and
    // the warm command.
    let builder_info = WorktreeInfo {
        root: info.root.clone(),
        git_dir: info.git_dir.clone(),
        head: base_ref.commit.clone(),
        branch: base_ref.branch.clone(),
    };
    let tree = sync::git_text(
        &info.root,
        &["rev-parse", &format!("{}^{{tree}}", base_ref.commit)],
    )?;
    let snapshot = Snapshot {
        tree,
        wip: base_ref.commit.clone(),
    };
    let (refs, digest) = sync::mirrored_refs(&builder_info, history)?;
    let mut seed = project.seed_config();
    let credentials = RunCredentials::gather(
        &wctx.workspace.root,
        project.git_identity(),
        &project.run_token_source(),
    );
    if let Some(token) = &credentials.github_token
        && seed.token.is_fetched()
    {
        seed.token = TokenSource::Fixed(token.clone());
    }
    let request = MirrorRequest {
        project: name,
        info: &builder_info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: up.member.sync.refs_digest.as_deref() != Some(digest.as_str()),
        known: usable_known(history, &up.member.sync.mode, &up.member.sync.known),
        history,
        seed: Some(&seed),
    };
    let host = crate::commands::runner_host(ctx, &up.target, &up.member, spec.clone());
    let warm = project
        .config()
        .base
        .warm
        .clone()
        .unwrap_or_else(|| "true".to_string());
    ctx.out
        .note("running the setup and ensure steps and [base] warm on the builder");
    let mut log = BuildLog::default();
    let outcome =
        sync::run_command_with(&host, &request, &warm, &credentials, &mut log, interrupts)?;
    drop(credentials);
    drop(seed);
    cancelled(interrupts)?;
    let exit_code = match outcome.end {
        RunEnd::Done { exit_code, .. } => exit_code,
        RunEnd::Lost { status } => {
            return Err(AppError::new(
                "connection",
                format!(
                    "the connection to the builder ended without a result{}",
                    status
                        .map(|status| format!(" (exit {status})"))
                        .unwrap_or_default()
                ),
            ));
        }
    };
    // What the builder's repository holds now, whatever the command did.
    let mut known: Vec<String> = Vec::new();
    for sha in &outcome.known {
        if !known.contains(sha) {
            known.push(sha.clone());
        }
    }
    let sync_state = SyncState {
        mode: history.as_str().to_string(),
        known: known.clone(),
        refs_digest: Some(digest.clone()),
    };
    let builder = pool.modify(&up.member, |member| {
        if !sync_state.known.is_empty() {
            member.sync = sync_state.clone();
        }
        member.last_used_at = system::now_ms();
    })?;
    if exit_code != 0 {
        let what = match &log.failed_step {
            Some(step) => format!("the {step} step"),
            None => "[base] warm".to_string(),
        };
        return Err(AppError::new(
            "base_build_failed",
            format!("{what} failed on the builder (exit {exit_code}); the base was not changed"),
        )
        .with_hint("its output is above; fix it and run `boatparty base build` again"));
    }
    phases.lap("warm");

    // 4. Settle.
    settle(&host, name, spec.hash())?;
    let settled = Instant::now();
    phases.lap("settle");

    // 5. Hydration.
    wait_hydrated(ctx, provider, &builder, &id, interrupts)?;
    let gap = wait::poll_interval().saturating_mul(SETTLE_GAP_POLLS);
    if let Some(rest) = gap.checked_sub(settled.elapsed()) {
        std::thread::sleep(rest);
    }
    phases.lap("hydration");

    // 6. Stop.
    if let Some(mut beat) = heartbeat.take() {
        beat.stop();
    }
    up.target.close_master();
    ctx.out
        .note(format_args!("stopping {} ({id})", builder.name));
    *stage = Stage::StopRequested;
    wait::retry(Duration::from_secs(60), wait::transient, || {
        provider.stop(&id)
    })?;
    wait_stopped(provider, &builder, &id, interrupts)?;
    phases.lap("stop");

    // 7 and 8. Save, then wait until it is ready.
    let snapshot_name = base::snapshot_name(name);
    ctx.out.note(format_args!(
        "saving {} ({id}) as {snapshot_name}",
        builder.name
    ));
    let size_bytes = save(ctx, provider, &id, &snapshot_name, interrupts)?;
    phases.lap("save");

    // 9. Record.
    let record = BaseRecord {
        snapshot: snapshot_name,
        commit: base_ref.commit.clone(),
        known,
        refs_digest: Some(digest),
        built_at: system::now_ms(),
        builder_id: id,
        mode: history.as_str().to_string(),
        git_ref: Some(base_ref.name.clone()),
        branch: base_ref.branch.clone(),
        size_bytes,
        extra: serde_json::Map::new(),
    };
    let value = record.to_value();
    pool.store.update(|state| {
        state.project_mut(name).base = Some(value);
        Ok(())
    })?;
    Ok((
        record,
        builder,
        outcome.pack_bytes,
        outcome.seed.route.as_str(),
    ))
}

/// The runner's output during a build goes to stderr; stdout is kept for
/// the result.
#[derive(Default)]
struct BuildLog {
    failed_step: Option<String>,
}

impl Sink for BuildLog {
    fn stdout(&mut self, bytes: &[u8]) {
        let _ = std::io::stderr().lock().write_all(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        let _ = std::io::stderr().lock().write_all(bytes);
    }

    fn event(&mut self, name: &str) {
        if let Some(step) = name.strip_prefix("failed-step ") {
            self.failed_step = Some(step.to_string());
        }
    }

    fn note(&mut self, message: &str) {
        eprintln!("boatparty: {message}");
    }
}

/// The runner's `settle`, installing the runner first when it is missing.
fn settle(host: &SshRunner, project: &str, spec: &str) -> Result<(), AppError> {
    let args: Vec<String> = ["settle", project, spec].map(String::from).to_vec();
    let mut installed = false;
    loop {
        let output = host
            .command(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| AppError::io("cannot start the runner", error))?;
        if output.status.code() == Some(EXIT_MISSING) && !installed {
            host.install()?;
            installed = true;
            continue;
        }
        let _ = std::io::stderr().lock().write_all(
            &output
                .stderr
                .split(|&b| b == b'\n')
                .filter(|line| !line.starts_with(b"boatparty-runner-missing"))
                .collect::<Vec<_>>()
                .join(&b'\n'),
        );
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("settled") {
            return Ok(());
        }
        return Err(AppError::new(
            "base_build_failed",
            format!(
                "settling the builder failed (exit {}); the base was not changed",
                output.status.code().unwrap_or(-1)
            ),
        ));
    }
}

fn hydration_timeout() -> Duration {
    std::env::var(HYDRATION_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map_or(HYDRATION_TIMEOUT, Duration::from_secs)
}

/// Wait until the builder runs with its files complete. Never stops it.
fn wait_hydrated(
    ctx: &Context,
    provider: &dyn Provider,
    builder: &Member,
    id: &str,
    interrupts: &Interrupts,
) -> Result<(), AppError> {
    let timeout = hydration_timeout();
    let deadline = Deadline::after(timeout);
    let mut said = false;
    loop {
        cancelled(interrupts)?;
        let sandbox = get(provider, id)?;
        match sandbox.state {
            SandboxState::Running if sandbox.hydration.is_complete() => return Ok(()),
            SandboxState::Running | SandboxState::Starting => {}
            state => {
                return Err(AppError::new(
                    "base_build_failed",
                    format!(
                        "{} ({id}) became {state} while the build waited for its files; the \
                         base was not changed",
                        builder.name
                    ),
                ));
            }
        }
        if !said {
            said = true;
            ctx.out.note(format_args!(
                "waiting for {} to report its files complete before stopping it",
                builder.name
            ));
        }
        if !deadline.tick() {
            return Err(AppError::new(
                "hydration_timeout",
                format!(
                    "{} ({id}) did not report its files complete within {}s; it was not \
                     stopped and the base was not changed",
                    builder.name,
                    timeout.as_secs()
                ),
            )
            .with_hint("run `boatparty base build` again later"));
        }
    }
}

fn wait_stopped(
    provider: &dyn Provider,
    builder: &Member,
    id: &str,
    interrupts: &Interrupts,
) -> Result<Sandbox, AppError> {
    let deadline = Deadline::after(STOP_TIMEOUT);
    loop {
        let sandbox = get(provider, id)?;
        if sandbox.state == SandboxState::Stopped {
            return Ok(sandbox);
        }
        if sandbox.state == SandboxState::Gone
            || sandbox.state == SandboxState::Error
            || !deadline.tick()
        {
            return Err(AppError::new(
                "stop_timeout",
                format!(
                    "{} ({id}) is {} rather than stopped; the base was not changed",
                    builder.name, sandbox.state
                ),
            ));
        }
        // A cancel here leaves a stopping builder, which is harmless.
        cancelled(interrupts)?;
    }
}

/// Save the stopped builder under `name` and wait until the save is ready.
/// Returns the snapshot's size.
fn save(
    ctx: &Context,
    provider: &dyn Provider,
    id: &str,
    name: &str,
    interrupts: &Interrupts,
) -> Result<Option<u64>, AppError> {
    let deadline = Deadline::after(SAVE_TIMEOUT);
    let mut said = false;
    // A rebuild replaces an existing base: its artifact must change before
    // the save counts as done, so a stale `ready` is never taken for it.
    let previous = provider
        .saved_snapshot(name)
        .ok()
        .flatten()
        .and_then(|snapshot| snapshot.artifact);
    loop {
        let saved = wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.save_snapshot(id, name)
        });
        match saved {
            Ok(_) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::Busy {
                        reason: BusyReason::SaveInProgress,
                        ..
                    }
                ) =>
            {
                if !said {
                    said = true;
                    ctx.out.note(format_args!(
                        "another save of {name} is in progress; waiting for it to settle"
                    ));
                }
                if !deadline.tick() {
                    return Err(error.into());
                }
                cancelled(interrupts)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    loop {
        let snapshot = wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.saved_snapshot(name)
        })?;
        match snapshot {
            Some(snapshot)
                if snapshot.status == SnapshotStatus::Ready
                    && (previous.is_none()
                        || snapshot.artifact.is_none()
                        || snapshot.artifact != previous) =>
            {
                return Ok(snapshot.size_bytes);
            }
            Some(snapshot) if snapshot.status == SnapshotStatus::Failed => {
                return Err(AppError::new(
                    "base_save_failed",
                    format!(
                        "saving {name} failed{}; the previous base, if any, is still in use",
                        snapshot
                            .error
                            .map(|error| format!(": {error}"))
                            .unwrap_or_default()
                    ),
                ));
            }
            _ => {}
        }
        if !deadline.tick() {
            return Err(AppError::new(
                "base_save_failed",
                format!(
                    "{name} was not ready within {} minutes; the previous base, if any, is still \
                     in use",
                    SAVE_TIMEOUT.as_secs() / 60
                ),
            ));
        }
    }
}

fn get(provider: &dyn Provider, id: &str) -> Result<Sandbox, AppError> {
    Ok(wait::retry(
        Duration::from_secs(60),
        wait::transient,
        || provider.get(id),
    )?)
}

/// An interrupt ends the build where it is.
fn cancelled(interrupts: &Interrupts) -> Result<(), AppError> {
    if interrupts.exit_status().is_some() {
        return Err(AppError::new(
            "interrupted",
            "the base build was interrupted; the base was not changed",
        ));
    }
    Ok(())
}

pub fn round(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// The build's phases as a JSON object of seconds.
pub fn phases_json(phases: &[(&'static str, f64)]) -> Value {
    Value::Object(
        phases
            .iter()
            .map(|(name, seconds)| (name.to_string(), json!(seconds)))
            .collect(),
    )
}
