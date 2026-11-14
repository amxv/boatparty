//! Bringing a member up: create or resume it, wait until it runs, then
//! authorize the tool's key and pin the host key.
//!
//! A member's state comes from the provider every time; nothing unknown is
//! resumed on a guess. Only `Stopped` (or `Stopping`) members are resumed.

use std::time::Duration;

use super::wait::{self, Deadline};
use super::{LABEL_PROJECT, Pool, member_labels, remote_checkout};
use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::{
    BusyReason, CreateSandbox, ErrorKind, MachineSize, Provider, ProviderError, Sandbox,
    SandboxState,
};
use crate::state::{BringingUp, Member, Role, SyncState};
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

pub fn bring_up(pool: &Pool<'_>, options: BringUp) -> Result<Up, AppError> {
    let (member, fresh) = pool.acquire()?;
    let member = pool.modify(&member, |member| {
        member.bringing_up = Some(BringingUp {
            pid: std::process::id(),
            started_at: system::now_ms(),
        });
    })?;
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

    let sandbox = 'outer: loop {
        let mut sandbox = match member.id.clone() {
            Some(id) => get(pool.provider, &id)?,
            None => {
                let sandbox = create(pool, &member, fresh, options)?;
                let id = sandbox.id.clone();
                member = pool.modify(&member, |member| member.id = Some(id))?;
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
                    match resume(pool.provider, &id, options.ttl_seconds)? {
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
    let (target, reconnected) = ensure_connected(
        pool.paths,
        pool.provider,
        pool.out,
        &pool.project,
        &sandbox.id,
        started,
    )?;
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

/// Resume once. `None` when the provider refused for now or the answer was
/// lost, in which case the caller reads the state again rather than resuming
/// blindly a second time.
fn resume(
    provider: &dyn Provider,
    id: &str,
    ttl_seconds: u64,
) -> Result<Option<crate::provider::Resumed>, AppError> {
    match provider.resume(id, ttl_seconds) {
        Ok(outcome) => Ok(Some(outcome)),
        Err(error) => match error.kind() {
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

/// Create the member's sandbox with its recorded idempotency key. A record
/// left pending by an earlier run is first looked up by name, in case that
/// run's create succeeded but its answer was lost.
fn create(
    pool: &Pool<'_>,
    member: &Member,
    fresh: bool,
    options: BringUp,
) -> Result<Sandbox, AppError> {
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
            return Ok(found);
        }
    }
    let request = CreateSandbox {
        name: member.name.clone(),
        size: options.size,
        ttl_seconds: options.ttl_seconds,
        labels: member_labels(&pool.project, Role::Pool),
        no_env: true,
        idempotency_key: member.create_key.clone(),
    };
    pool.out.note(format_args!(
        "creating {} ({} machine)",
        member.name, options.size
    ));
    let mut ambiguous = !fresh;
    let result = wait::retry(CREATE_RETRY, create_retryable, || {
        let result = pool.provider.create(&request);
        if let Err(error) = &result
            && wait::transient(error.kind())
        {
            ambiguous = true;
        }
        result
    });
    match result {
        Ok(sandbox) => Ok(sandbox),
        Err(error) => {
            // A definite refusal of the only attempt created nothing, so the
            // pending record can go. After an unclear outcome it stays, and
            // the next run reuses its key.
            if !ambiguous && refused(&error) {
                pool.forget(member)?;
            }
            Err(error.into())
        }
    }
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

/// Make sure the member is reachable: when `force` (after a start) or when
/// the control master is not alive, authorize the tool's key, pin the host
/// key the provider reports, and open a new master.
pub fn ensure_connected(
    paths: &Paths,
    provider: &dyn Provider,
    out: crate::output::Output,
    project: &str,
    id: &str,
    force: bool,
) -> Result<(SshTarget, bool), AppError> {
    let target = SshTarget::for_sandbox(paths, id);
    if !force && target.master_alive() {
        return Ok((target, false));
    }
    let public_key = ssh::ensure_key(paths)?;
    let access = wait::retry(SSH_KEY_RETRY, wait::ssh_not_ready, || {
        provider.authorize_ssh_key(id, &public_key)
    })?;
    // The old master, if any, talks to the previous machine.
    target.close_master();
    ssh::write_target(paths, &target, &access)?;
    let remote = format!("mkdir -p {}", shell_quote(&remote_checkout(project)));
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
