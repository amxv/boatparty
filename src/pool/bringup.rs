//! Bringing a member up: create or resume it, wait until it runs, then
//! authorize the tool's key and pin the host key.
//!
//! A member's state comes from the provider every time; nothing unknown is
//! resumed on a guess. Only `Stopped` (or `Stopping`) members are resumed.

use super::recovery;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::wait::{self, Deadline};
use super::{LABEL_PROJECT, LeaseRequest, Pool, member_labels, new_member};
use crate::base::{self, BaseRecord};
use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::{
    BusyReason, CreateSandbox, ErrorKind, MachineSize, Provider, ProviderError, Sandbox,
    SandboxState,
};
use crate::starts::StartFailure;
use crate::state::{BringingUp, Lease, LeaseKind, Member, Role, SyncState};
use crate::system;
use crate::transport::ssh::{self, SshTarget, shell_quote};

const START_TIMEOUT: Duration = Duration::from_secs(600);
const CREATE_RETRY: Duration = Duration::from_secs(180);
const SSH_KEY_RETRY: Duration = Duration::from_secs(180);
const CONNECT_ATTEMPTS: u32 = 6;

/// How to bring a member up.
#[derive(Debug, Clone, Copy)]
pub struct BringUp {
    pub size: MachineSize,
    /// The auto-stop length set on create and resume.
    pub ttl_seconds: u64,
    /// Also push the auto-stop deadline of a member that is already running.
    pub extend_running: bool,
}

/// A member that is running and reachable over SSH.
#[derive(Debug, Clone)]
pub struct Up {
    pub member: Member,
    pub sandbox: Sandbox,
    pub target: SshTarget,
    pub created: bool,
    pub resumed: bool,
    /// The resume took back the machine kept after the stop.
    pub reclaimed: bool,
    /// The key was authorized and the host key pinned again.
    pub reconnected: bool,
    pub restore_wait: Duration,
}

pub fn bring_up(
    pool: &Pool<'_>,
    options: BringUp,
    lease: Option<&LeaseRequest>,
) -> Result<Up, AppError> {
    let result = bring_up_inner(pool, options, lease);
    if result.is_err() && lease.is_some() {
        // Let go of the lease taken with the member; the caller has nothing to run.
        let pid = std::process::id();
        let project = pool.project.clone();
        let _ = pool.store.update(|state| {
            for member in &mut state.project_mut(&project).members {
                member.leases.retain(|lease| lease.pid != pid);
            }
            Ok(())
        });
    }
    result
}

/// Bring the project's builder up for a base build, creating its record
/// (role `builder`, no affinity) on first use. This process holds it with
/// a lease until the build releases it; another live build is refused.
pub fn bring_up_builder(pool: &Pool<'_>, options: BringUp) -> Result<Up, AppError> {
    let project = pool.project.clone();
    let key = system::uuid_v4()?;
    let now = system::now_ms();
    let pid = std::process::id();
    let claim = pool.store.update(|state| {
        let slot = state.project_mut(&project);
        let index = match slot
            .members
            .iter()
            .position(|member| member.role == Role::Builder)
        {
            Some(index) => index,
            None => {
                let mut member = new_member(base::builder_name(&project), key.clone(), None, now);
                member.role = Role::Builder;
                slot.members.push(member);
                slot.members.len() - 1
            }
        };
        let member = &mut slot.members[index];
        let fresh = member.create_key == key;
        if member.busy() {
            return Ok(Err(member.clone()));
        }
        member
            .leases
            .retain(|lease| lease.pid != pid && system::pid_alive(lease.pid));
        member.leases.push(Lease {
            pid,
            kind: LeaseKind::Session,
            label: "base build".to_string(),
            started_at: now,
            workspace: None,
        });
        member.bringing_up = Some(BringingUp::new(pid, now));
        Ok(Ok((member.clone(), fresh)))
    })?;
    let (member, fresh) = claim.map_err(|busy| {
        AppError::new(
            "base_busy",
            format!(
                "{} is in use by another boatparty process ({})",
                busy.name,
                busy.live_leases()
                    .map(|lease| format!("pid {} {}", lease.pid, lease.label))
                    .chain(
                        busy.bringing_up
                            .iter()
                            .map(|claim| format!("pid {} starting it", claim.pid))
                    )
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_hint("wait for that base build to finish")
    })?;
    let result = claimed(pool, member, fresh, options);
    if result.is_err() {
        release_builder(pool);
    }
    result
}

/// Drop this process's lease on the builder.
pub fn release_builder(pool: &Pool<'_>) {
    let pid = std::process::id();
    let project = pool.project.clone();
    let _ = pool.store.update(|state| {
        for member in &mut state.project_mut(&project).members {
            if member.role == Role::Builder {
                member.leases.retain(|lease| lease.pid != pid);
            }
        }
        Ok(())
    });
}

fn bring_up_inner(
    pool: &Pool<'_>,
    options: BringUp,
    lease: Option<&LeaseRequest>,
) -> Result<Up, AppError> {
    loop {
        let (member, fresh) = pool.acquire(lease)?;
        if member
            .bringing_up
            .as_ref()
            .is_some_and(|c| c.pid == std::process::id())
        {
            return claimed(pool, member, fresh, options);
        }
        let began = Instant::now();
        let waited = wait_for_claim(pool, &member);
        pool.waited.set(pool.waited.get() + began.elapsed());
        waited?;
    }
}

/// Bring up a member this process holds the claim on, then clear the claim.
fn claimed(pool: &Pool<'_>, member: Member, fresh: bool, options: BringUp) -> Result<Up, AppError> {
    let claim = member.bringing_up.clone().expect("bring-up claim");
    let name = member.name.clone();
    let result = recovery::bring_up(pool, member, fresh, options);
    let cleared = pool.store.update(|state| {
        let project = state.project_mut(&pool.project);
        if let Err(error) = &result {
            project.bring_up_failures.insert(
                name.clone(),
                crate::state::BringUpFailure {
                    pid: claim.pid,
                    started_at: claim.started_at,
                    message: error.to_string(),
                },
            );
        }
        if let Some(slot) = project.members.iter_mut().find(|m| m.name == name)
            && slot
                .bringing_up
                .as_ref()
                .is_some_and(|c| c.pid == claim.pid && c.started_at == claim.started_at)
        {
            slot.bringing_up = None;
            return Ok(Some(slot.clone()));
        }
        Ok(None)
    })?;
    let mut up = result?;
    if let Some(member) = cleared {
        up.member = member;
    }
    Ok(up)
}

fn wait_for_claim(pool: &Pool<'_>, member: &Member) -> Result<(), AppError> {
    let original = member.bringing_up.as_ref().expect("other claim");
    let mut reported = None;
    let mut last = Instant::now();
    loop {
        let state = pool.store.load()?;
        let Some(project) = state.project(&pool.project) else {
            return Ok(());
        };
        if let Some(failure) = project.bring_up_failures.get(&member.name)
            && failure.pid == original.pid
            && failure.started_at == original.started_at
        {
            return Err(AppError::new(
                "member_failed",
                format!(
                    "{} bring-up by pid {} failed: {}",
                    member.name, original.pid, failure.message
                ),
            ));
        }
        let Some(claim) = project
            .find(&member.name)
            .and_then(|m| m.bringing_up.as_ref())
            .filter(|claim| {
                claim.pid == original.pid
                    && claim.started_at == original.started_at
                    && system::pid_alive(claim.pid)
            })
        else {
            return Ok(());
        };
        let step = if claim.step.is_empty() {
            "starting sandbox"
        } else {
            &claim.step
        };
        if claim.wedged(system::now_ms()) {
            return Err(AppError::new(
                "start_timeout",
                format!(
                    "{} bring-up holder pid {} looks wedged during {step} (limit {}s)",
                    member.name, claim.pid, claim.limit_seconds
                ),
            ));
        }
        if reported.as_deref() != Some(step) || last.elapsed() >= Duration::from_secs(60) {
            pool.out.note(format_args!(
                "another boatparty process is starting {}; waiting for pid {}: {step} (limit {}s)",
                member.name, claim.pid, claim.limit_seconds
            ));
            reported = Some(step.to_string());
            last = Instant::now();
        }
        std::thread::sleep(wait::poll_interval());
    }
}

pub(super) fn step(
    pool: &Pool<'_>,
    member: &Member,
    name: &str,
    limit: Duration,
) -> Result<(), AppError> {
    pool.modify(member, |m| {
        if let Some(c) = m.bringing_up.as_mut() {
            c.step = name.into();
            c.since = system::now_ms();
            c.limit_seconds = limit.as_secs();
        }
    })?;
    Ok(())
}

pub(super) fn run(
    pool: &Pool<'_>,
    mut member: Member,
    fresh: bool,
    options: BringUp,
) -> Result<Up, AppError> {
    let mut fresh = fresh;
    let mut created = false;
    let mut resumed = false;
    let mut reclaimed = false;
    let mut recreated = false;
    let mut deadline = Deadline::after(START_TIMEOUT);
    let sandbox = 'outer: loop {
        let mut sandbox = match member.id.clone() {
            Some(id) => get(pool.provider, &id)?,
            None => {
                step(
                    pool,
                    &member,
                    "admitting and creating sandbox",
                    crate::starts::ADMISSION_LIMIT + CREATE_RETRY,
                )?;
                let (sandbox, record, base) = create(pool, &member, fresh, options)?;
                let id = sandbox.id.clone();
                member = pool.modify(&record, |member| {
                    member.id = Some(id);
                    member.recovery.restoring = true;
                    // The base's repository is this sandbox's: the first
                    // sync sends only what changed since the base commit.
                    if let Some(base) = &base {
                        member.sync = base.seeded_sync();
                    }
                })?;
                deadline = Deadline::after(START_TIMEOUT);
                created = true;
                sandbox
            }
        };
        let id = sandbox.id.clone();
        step(pool, &member, "waiting for running sandbox", START_TIMEOUT)?;
        loop {
            match sandbox.state {
                SandboxState::Running => break 'outer sandbox,
                SandboxState::Gone => {
                    if recreated || created {
                        return Err(AppError::new(
                            "provider_error",
                            format!("sandbox {id} disappeared while it was being started"),
                        ));
                    }
                    pool.out.note(format_args!(
                        "{} ({id}) no longer exists at the provider; creating a new one",
                        member.name
                    ));
                    let key = system::uuid_v4()?;
                    member = pool.modify(&member, |member| {
                        member.id = None;
                        member.create_key = key;
                        member.sync = SyncState::default();
                        member.created_at = system::now_ms();
                        member.recovery = Default::default();
                    })?;
                    fresh = true;
                    pool.recovery.set(Some("replaced"));
                    recreated = true;
                    continue 'outer;
                }
                SandboxState::Error => {
                    return Err(AppError::new(
                        "member_failed",
                        format!(
                            "{} ({id}) is in the error state{}",
                            member.name,
                            sandbox
                                .error
                                .as_deref()
                                .map(|error| format!(": {error}"))
                                .unwrap_or_default()
                        ),
                    )
                    .with_hint(format!("delete it with `boatparty rm {}`", member.name)));
                }
                SandboxState::Stopped | SandboxState::Stopping if !resumed => {
                    if sandbox.data_expired_at.is_some() {
                        return Err(AppError::new(
                            "data_expired",
                            format!(
                                "{} ({id}) was stopped so long that the provider deleted its disk",
                                member.name
                            ),
                        )
                        .with_hint(format!("delete it with `boatparty rm {}`", member.name)));
                    }
                    pool.out
                        .note(format_args!("resuming {} ({id})", member.name));
                    step(
                        pool,
                        &member,
                        "admitting and resuming sandbox",
                        crate::starts::ADMISSION_LIMIT + START_TIMEOUT,
                    )?;
                    pool.modify(&member, |m| m.recovery.restoring = true)?;
                    match resume(pool, &member.name, &id, options.ttl_seconds)? {
                        Some(outcome) => {
                            step(pool, &member, "waiting for running sandbox", START_TIMEOUT)?;
                            deadline = Deadline::after(START_TIMEOUT);
                            resumed = true;
                            reclaimed = outcome.reclaimed;
                            sandbox = match outcome.sandbox {
                                Some(next) if next.state == SandboxState::Running => next,
                                _ => get(pool.provider, &id)?,
                            };
                        }
                        // Refused for now (still stopping) or the answer was lost.
                        None => {
                            if deadline.passed() {
                                return Err(AppError::new(
                                    "start_timeout",
                                    format!(
                                        "resuming {} timed out: {}",
                                        member.name,
                                        sandbox.condition()
                                    ),
                                ));
                            }
                            sandbox = get(pool.provider, &id)?;
                        }
                    }
                }
                SandboxState::Starting | SandboxState::Stopped | SandboxState::Stopping => {
                    if !deadline.tick() {
                        return Err(AppError::new(
                            "start_timeout",
                            format!(
                                "waiting for {} to run: {} after {} minutes",
                                member.name,
                                sandbox.condition(),
                                START_TIMEOUT.as_secs() / 60
                            ),
                        ));
                    }
                    sandbox = get(pool.provider, &id)?;
                }
            }
        }
    };

    let started = created || resumed;
    if !started && options.extend_running {
        // Best effort: a failed extension leaves the old deadline in place.
        if let Err(error) = wait::retry(Duration::from_secs(20), wait::transient, || {
            pool.provider.set_ttl(&sandbox.id, options.ttl_seconds)
        }) {
            pool.out.note(format_args!(
                "warning: could not push the auto-stop of {} forward: {error}",
                member.name
            ));
        }
    }
    let wait_for_restore = member
        .leases
        .iter()
        .any(|l| l.pid == std::process::id() && l.kind == LeaseKind::Run)
        && (started || member.recovery.restoring || !sandbox.hydration.is_complete());
    let (sandbox, restore_wait) = if wait_for_restore {
        recovery::wait_restore(pool, &member, sandbox, options.ttl_seconds)?
    } else {
        (sandbox, Duration::ZERO)
    };
    if sandbox.degraded_since.is_some() {
        pool.out.note(format_args!(
            "{}: {}. Using direct SSH",
            member.name,
            sandbox.condition()
        ));
    }
    step(
        pool,
        &member,
        "authorizing SSH and opening connection",
        Duration::from_secs(420),
    )?;
    let pinned = SshTarget::for_sandbox(pool.paths, &sandbox.id);
    let (target, reconnected) = if sandbox.degraded_since.is_some()
        && pinned.config.exists()
        && matches!(pinned.run_quiet("true"), Ok((0, _)))
    {
        (pinned, false)
    } else {
        connect_steps(
            pool.paths,
            pool.provider,
            pool.out,
            &sandbox.id,
            started,
            |name, limit| step(pool, &member, name, limit),
        )
        .map_err(|e| {
            let condition = pool
                .provider
                .get(&sandbox.id)
                .map(|s| s.condition())
                .unwrap_or_else(|_| format!("last observed {}", sandbox.condition()));
            e.with_hint(condition)
        })?
    };
    let member = pool.modify(&member, |member| {
        member.last_used_at = system::now_ms();
        member.recovery.restoring = !sandbox.hydration.is_complete();
    })?;
    Ok(Up {
        member,
        sandbox,
        target,
        created,
        resumed,
        reclaimed,
        reconnected,
        restore_wait,
    })
}

pub(super) fn get(provider: &dyn Provider, id: &str) -> Result<Sandbox, AppError> {
    Ok(wait::retry(
        Duration::from_secs(60),
        wait::transient,
        || provider.get(id),
    )?)
}

/// Resume once, admitted by the start ledger. `None` when the provider
/// refused for now or the answer was lost, in which case the caller reads
/// the state again rather than resuming blindly a second time. A resume
/// that takes back the kept machine is not a start and keeps no reservation.
fn resume(
    pool: &Pool<'_>,
    name: &str,
    id: &str,
    ttl_seconds: u64,
) -> Result<Option<crate::provider::Resumed>, AppError> {
    let started = pool.start_gate().start(
        name,
        || pool.provider.resume(id, ttl_seconds),
        |resumed| !resumed.reclaimed,
    );
    match started {
        Ok(outcome) => Ok(Some(outcome)),
        Err(StartFailure::Refused(error)) => Err(error),
        Err(StartFailure::Provider(error)) => match error.kind() {
            ErrorKind::Busy {
                reason: BusyReason::Stopping | BusyReason::Resuming | BusyReason::Starting,
                ..
            } => {
                std::thread::sleep(wait::retry_delay(&error, 0));
                Ok(None)
            }
            kind if wait::transient(kind) => {
                // The reply could have been lost after boat started the machine.
                let observed = get(pool.provider, id)?;
                if matches!(
                    observed.state,
                    SandboxState::Starting | SandboxState::Running
                ) {
                    Ok(Some(crate::provider::Resumed {
                        reclaimed: false,
                        sandbox: Some(observed),
                    }))
                } else {
                    Err(error.into())
                }
            }
            _ => Err(error.into()),
        },
    }
}

/// The base a new member of this role starts from: the project's, for
/// pool members only (the builder starts blank).
fn base_for(pool: &Pool<'_>, role: Role) -> Result<Option<BaseRecord>, AppError> {
    if role != Role::Pool {
        return Ok(None);
    }
    let state = pool.store.load()?;
    Ok(state.project(&pool.project).and_then(base::record))
}

/// Create the member's sandbox with its recorded idempotency key, admitted
/// by the start ledger, from the project's base when there is one. A record
/// left pending by an earlier run is first looked up by name, in case that
/// run's create succeeded but its answer was lost. Returns the sandbox, the
/// record (whose key may have been renewed after a rate-limited attempt)
/// and the base it started from.
fn create(
    pool: &Pool<'_>,
    member: &Member,
    fresh: bool,
    options: BringUp,
) -> Result<(Sandbox, Member, Option<BaseRecord>), AppError> {
    let mut base = base_for(pool, member.role)?;
    if let Some(record) = &base
        && let Ok(None) = pool.provider.saved_snapshot(&record.snapshot)
    {
        // Removed outside boatparty: start blank rather than fail.
        pool.out.note(format_args!(
            "the base {} no longer exists at the provider; creating {} without it (`boatparty              base build` makes a new one)",
            record.snapshot, member.name
        ));
        base = None;
    }
    if !fresh {
        let existing = wait::retry(Duration::from_secs(60), wait::transient, || {
            pool.provider.list_by_label(LABEL_PROJECT, &pool.project)
        })?;
        let mut matches = existing.into_iter().filter(|sandbox| {
            sandbox.name.as_deref() == Some(member.name.as_str())
                && sandbox.state != SandboxState::Gone
        });
        if let (Some(found), None) = (matches.next(), matches.next()) {
            pool.out.note(format_args!(
                "found {} ({}) from an earlier interrupted create",
                member.name, found.id
            ));
            // It may predate the base; a wrong guess costs one full pack.
            return Ok((found, member.clone(), base));
        }
    }
    match &base {
        Some(base) => pool.out.note(format_args!(
            "creating {} ({} machine) from the base {}",
            member.name, options.size, base.snapshot
        )),
        None => pool.out.note(format_args!(
            "creating {} ({} machine)",
            member.name, options.size
        )),
    }
    let record = RefCell::new(member.clone());
    let mut ambiguous = !fresh;
    let mut rate_limited = false;
    let result = pool.start_gate().start(
        &member.name,
        || {
            if rate_limited {
                // Nothing was created under the refused key; a fresh key
                // keeps a remembered refusal from answering the retry.
                let renewed = renew_key(pool, &record.borrow()).map_err(|error| {
                    ProviderError::new(ErrorKind::Unknown).with_message(error.to_string())
                })?;
                *record.borrow_mut() = renewed;
                rate_limited = false;
            }
            let request = CreateSandbox {
                name: member.name.clone(),
                size: options.size,
                ttl_seconds: options.ttl_seconds,
                labels: member_labels(&pool.project, member.role),
                no_env: pool.environment.is_none(),
                environment: pool.environment.clone(),
                idempotency_key: record.borrow().create_key.clone(),
                from: base.as_ref().map(|base| base.snapshot.clone()),
            };
            let result = wait::retry(CREATE_RETRY, create_retryable, || {
                let result = pool.provider.create(&request);
                if let Err(error) = &result
                    && wait::transient(error.kind())
                {
                    ambiguous = true;
                }
                result
            });
            if let Err(error) = &result
                && matches!(error.kind(), ErrorKind::RateLimited(_))
            {
                rate_limited = true;
            }
            result
        },
        |_| true,
    );
    let record = record.into_inner();
    match result {
        Ok(sandbox) => Ok((sandbox, record, base)),
        // A definite refusal of the only attempt created nothing, so the
        // pending record can go. After an unclear outcome it stays, and the
        // next run reuses its key.
        Err(StartFailure::Provider(error)) => {
            let forget = !ambiguous && refused(&error);
            let error = AppError::from(error);
            if forget {
                forget_failure(pool, &record, &error)?;
            }
            Err(error)
        }
        Err(StartFailure::Refused(error)) => {
            if !ambiguous {
                forget_failure(pool, &record, &error)?;
            }
            Err(error)
        }
    }
}

/// Publish the holder's error atomically with removal so a waiter never
/// observes a vanished record before it can read why creation failed.
fn forget_failure(pool: &Pool<'_>, member: &Member, error: &AppError) -> Result<(), AppError> {
    pool.store.update(|state| {
        let project = state.project_mut(&pool.project);
        if let Some(claim) = &member.bringing_up {
            project.bring_up_failures.insert(
                member.name.clone(),
                crate::state::BringUpFailure {
                    pid: claim.pid,
                    started_at: claim.started_at,
                    message: error.to_string(),
                },
            );
        }
        project.remove_by_key(&member.create_key);
        Ok(())
    })
}

/// Give a pending record a new create key.
fn renew_key(pool: &Pool<'_>, member: &Member) -> Result<Member, AppError> {
    let key = system::uuid_v4()?;
    let project = pool.project.clone();
    pool.store.update(|state| {
        let slot = state
            .project_mut(&project)
            .find_mut_by_key(&member.create_key)
            .ok_or_else(|| AppError::new("member_failed", "the pending record disappeared"))?;
        slot.create_key = key;
        Ok(slot.clone())
    })
}

fn create_retryable(kind: &ErrorKind) -> bool {
    wait::transient(kind)
        || matches!(
            kind,
            ErrorKind::Busy {
                reason: BusyReason::IdempotencyInProgress,
                ..
            } | ErrorKind::AtCapacity { .. }
        )
}

fn refused(error: &ProviderError) -> bool {
    error
        .status()
        .is_some_and(|status| (400..500).contains(&status))
        && !matches!(error.kind(), ErrorKind::Busy { .. })
}

/// Ask the provider to delete a broken member's sandbox, without waiting
/// for the deletion to finish (a sandbox that is already gone is fine), and
/// forget its SSH files.
pub(super) fn discard(pool: &Pool<'_>, name: &str, id: &str) -> Result<(), AppError> {
    let target = SshTarget::for_sandbox(pool.paths, id);
    target.close_master();
    match wait::retry(Duration::from_secs(60), wait::transient, || {
        pool.provider.delete(id)
    }) {
        Ok(_) => {}
        Err(error) if matches!(error.kind(), ErrorKind::NotFound) => {}
        Err(error) => {
            return Err(AppError::from(error).with_hint(format!(
                "delete {name} with `boatparty rm {name}`, then run the command again"
            )));
        }
    }
    target.remove_files();
    Ok(())
}

/// Make sure the member is reachable: when `force` (after a start) or when
/// the control master is not alive, authorize the tool's key, pin the host
/// key the provider reports, and open a new master.
pub fn ensure_connected(
    paths: &Paths,
    provider: &dyn Provider,
    out: crate::output::Output,
    id: &str,
    force: bool,
) -> Result<(SshTarget, bool), AppError> {
    connect_steps(paths, provider, out, id, force, |_, _| Ok(()))
}

fn connect_steps(
    paths: &Paths,
    provider: &dyn Provider,
    out: crate::output::Output,
    id: &str,
    force: bool,
    progress: impl Fn(&str, Duration) -> Result<(), AppError>,
) -> Result<(SshTarget, bool), AppError> {
    let target = SshTarget::for_sandbox(paths, id);
    if !force && target.reachable() {
        return Ok((target, false));
    }
    progress(
        "authorizing SSH key",
        SSH_KEY_RETRY + Duration::from_secs(120),
    )?;
    let public_key = ssh::ensure_key(paths)?;
    let access = wait::retry(SSH_KEY_RETRY, wait::ssh_not_ready, || {
        provider.authorize_ssh_key(id, &public_key)
    })?;
    progress("opening SSH connection", Duration::from_secs(210))?;
    // The old master, if any, talks to the previous machine.
    target.close_master();
    ssh::write_target(paths, &target, &access)?;
    // Proves the connection; the runner makes the checkout itself, wherever
    // the project's remote_path puts it.
    let remote = format!("mkdir -p {}", shell_quote(crate::runner::REMOTE_STATE_DIR));
    let mut last = String::new();
    for attempt in 0..CONNECT_ATTEMPTS {
        let (code, stderr) = match target.run_quiet(&remote) {
            Err(e) if e.code() == "step_timeout" => (255, e.to_string()),
            result => result?,
        };
        if code != 255 && code != 0 {
            return Err(AppError::new(
                "ssh",
                format!("initializing runner directory on {id} failed: {stderr}"),
            ));
        }
        if code == 0 {
            return Ok((target, true));
        }
        last = stderr;
        if attempt + 1 < CONNECT_ATTEMPTS {
            out.note(format_args!("SSH to {id} is not ready yet; retrying"));
            std::thread::sleep(wait::poll_interval().saturating_mul(attempt + 1));
        }
    }
    Err(AppError::new(
        "ssh_unreachable",
        format!("cannot connect to {id} over SSH: {last}"),
    ))
}
