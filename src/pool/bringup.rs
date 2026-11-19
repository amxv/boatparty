//! Bringing a member up: create or resume it, wait until it runs, then
//! authorize the tool's key and pin the host key.
//!
//! A member's state comes from the provider every time; nothing unknown is
//! resumed on a guess. Only `Stopped` (or `Stopping`) members are resumed.

use std::cell::RefCell;
use std::time::Duration;

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
        member.bringing_up = Some(BringingUp {
            pid,
            started_at: now,
        });
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

/// The single-flight claim on a member's bring-up.
enum Claim {
    Mine(Member),
    /// Another live process is bringing it up.
    Theirs(Member),
    /// The record went away meanwhile.
    Lost,
}

fn bring_up_inner(
    pool: &Pool<'_>,
    options: BringUp,
    lease: Option<&LeaseRequest>,
) -> Result<Up, AppError> {
    let deadline = Deadline::after(START_TIMEOUT);
    let project = pool.project.clone();
    let mut announced = false;
    loop {
        let (member, fresh) = pool.acquire(lease)?;
        let claim = pool.store.update(|state| {
            let Some(slot) = state
                .project_mut(&project)
                .find_mut_by_key(&member.create_key)
            else {
                return Ok(Claim::Lost);
            };
            if slot.bring_up_claimed_by_other() {
                return Ok(Claim::Theirs(slot.clone()));
            }
            slot.bringing_up = Some(BringingUp {
                pid: std::process::id(),
                started_at: system::now_ms(),
            });
            Ok(Claim::Mine(slot.clone()))
        })?;
        match claim {
            Claim::Mine(member) => return claimed(pool, member, fresh, options),
            Claim::Lost => continue,
            Claim::Theirs(member) => {
                if !announced {
                    announced = true;
                    pool.out.note(format_args!(
                        "another boatparty process is starting {}; waiting for it",
                        member.name
                    ));
                }
                // Wait for that claim to end, then acquire again: the member
                // is then up and its connection is reused.
                loop {
                    let state = pool.store.load()?;
                    let still = state
                        .project(&project)
                        .and_then(|pool| {
                            pool.members
                                .iter()
                                .find(|other| other.create_key == member.create_key)
                        })
                        .is_some_and(Member::bring_up_claimed_by_other);
                    if !still {
                        break;
                    }
                    if !deadline.tick() {
                        return Err(AppError::new(
                            "start_timeout",
                            format!(
                                "{} was still being started by another process after {} minutes",
                                member.name,
                                START_TIMEOUT.as_secs() / 60
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Bring up a member this process holds the claim on, then clear the claim.
fn claimed(pool: &Pool<'_>, member: Member, fresh: bool, options: BringUp) -> Result<Up, AppError> {
    let result = run(pool, member.clone(), fresh, options);
    // Clear the claim whatever happened; the record may have been replaced.
    let key = match &result {
        Ok(up) => up.member.clone(),
        Err(_) => member,
    };
    let project = pool.project.clone();
    let cleared = pool.store.update(|state| {
        let pool = state.project_mut(&project);
        if let Some(slot) = pool.find_mut_by_key(&key.create_key) {
            slot.bringing_up = None;
            return Ok(Some(slot.clone()));
        }
        Ok(None)
    });
    let mut up = result?;
    if let Ok(Some(member)) = cleared {
        up.member = member;
    }
    Ok(up)
}

fn run(pool: &Pool<'_>, mut member: Member, fresh: bool, options: BringUp) -> Result<Up, AppError> {
    let mut fresh = fresh;
    let mut created = false;
    let mut resumed = false;
    let mut reclaimed = false;
    let mut recreated = false;
    let deadline = Deadline::after(START_TIMEOUT);

    if let (Some(broken), Some(id)) = (member.broken.clone(), member.id.clone()) {
        let me = std::process::id();
        let refused = if member.live_leases().any(|lease| lease.pid != me) {
            pool.out.note(format_args!(
                "{} is broken ({}) but still in use; it is replaced once that ends",
                member.name, broken.reason
            ));
            true
        } else if let Some(error) = pool.start_gate().refused_today() {
            // Deleting it now would leave this workspace with nothing.
            pool.out.note(format_args!(
                "{} is broken ({}), but a replacement cannot start today ({}); using it as it is",
                member.name,
                broken.reason,
                error.message()
            ));
            true
        } else {
            false
        };
        if !refused {
            pool.out.note(format_args!(
                "{} ({id}) is broken: {}; deleting it and creating a new sandbox in its place",
                member.name, broken.reason
            ));
            discard(pool, &member.name, &id)?;
            let key = system::uuid_v4()?;
            member = pool.modify(&member, |member| {
                member.id = None;
                member.create_key = key;
                member.sync = SyncState::default();
                member.created_at = system::now_ms();
                member.broken = None;
            })?;
            fresh = true;
        }
    }

    let sandbox = 'outer: loop {
        let mut sandbox = match member.id.clone() {
            Some(id) => get(pool.provider, &id)?,
            None => {
                let (sandbox, record, base) = create(pool, &member, fresh, options)?;
                let id = sandbox.id.clone();
                member = pool.modify(&record, |member| {
                    member.id = Some(id);
                    // The base's repository is this sandbox's: the first
                    // sync sends only what changed since the base commit.
                    if let Some(base) = &base {
                        member.sync = base.seeded_sync();
                    }
                })?;
                created = true;
                sandbox
            }
        };
        let id = sandbox.id.clone();
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
                    })?;
                    fresh = true;
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
                    match resume(pool, &member.name, &id, options.ttl_seconds)? {
                        Some(outcome) => {
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
                                    format!("{} ({id}) could not be resumed in time", member.name),
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
                                "{} ({id}) is still {} after {} minutes",
                                member.name,
                                sandbox.state,
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
    let (target, reconnected) =
        ensure_connected(pool.paths, pool.provider, pool.out, &sandbox.id, started)?;
    let member = pool.modify(&member, |member| {
        member.last_used_at = system::now_ms();
    })?;
    Ok(Up {
        member,
        sandbox,
        target,
        created,
        resumed,
        reclaimed,
        reconnected,
    })
}

fn get(provider: &dyn Provider, id: &str) -> Result<Sandbox, AppError> {
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
                std::thread::sleep(wait::retry_delay(&error, 0));
                Ok(None)
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
            if !ambiguous && refused(&error) {
                pool.forget(&record)?;
            }
            Err(error.into())
        }
        Err(StartFailure::Refused(error)) => {
            if !ambiguous {
                pool.forget(&record)?;
            }
            Err(error)
        }
    }
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
fn discard(pool: &Pool<'_>, name: &str, id: &str) -> Result<(), AppError> {
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
    let target = SshTarget::for_sandbox(paths, id);
    if !force && target.reachable() {
        return Ok((target, false));
    }
    let public_key = ssh::ensure_key(paths)?;
    let access = wait::retry(SSH_KEY_RETRY, wait::ssh_not_ready, || {
        provider.authorize_ssh_key(id, &public_key)
    })?;
    // The old master, if any, talks to the previous machine.
    target.close_master();
    ssh::write_target(paths, &target, &access)?;
    // Proves the connection; the runner makes the checkout itself, wherever
    // the project's remote_path puts it.
    let remote = format!("mkdir -p {}", shell_quote(crate::runner::REMOTE_STATE_DIR));
    let mut last = String::new();
    for attempt in 0..CONNECT_ATTEMPTS {
        let (code, stderr) = target.run_quiet(&remote)?;
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
        "ssh",
        format!("cannot connect to {id} over SSH: {last}"),
    ))
}
