//! Recovery holds the same claim through a restart or replacement. Provider
//! transport and server failures never establish that a machine is stuck.
use super::{
    BringUp, Pool, Up,
    bringup::{discard, get, run, step},
    wait,
};
use crate::error::AppError;
use crate::provider::{ErrorKind, Hydration, Sandbox, SandboxState};
use crate::state::{Member, SyncState};
use crate::{system, transport::ssh::SshTarget};
use std::time::{Duration, Instant};

pub(super) const COOLDOWN_MS: u64 = 30 * 60_000;

fn restart_snapshot(sandbox: &Sandbox, last: Option<u64>, now: u64) -> Option<u64> {
    if sandbox.hydration.is_complete()
        && matches!(
            sandbox.state,
            SandboxState::Running
                | SandboxState::Starting
                | SandboxState::Stopping
                | SandboxState::Error
        )
        && last.is_none_or(|at| now.saturating_sub(at) >= COOLDOWN_MS)
    {
        sandbox.snapshot_seconds
    } else {
        None
    }
}

pub(super) fn bring_up(
    pool: &Pool<'_>,
    mut member: Member,
    fresh: bool,
    options: BringUp,
) -> Result<Up, AppError> {
    // Older helpers marked tunnel-only degradation broken. Re-probe SSH.
    if member.broken.as_ref().is_some_and(|b| {
        b.reason
            .starts_with("the provider could not reach its machine for ")
    }) {
        member = pool.modify(&member, |m| m.broken = None)?;
    }
    let reason = if let Some(broken) = &member.broken {
        broken.reason.clone()
    } else {
        match run(pool, member.clone(), fresh, options) {
            Ok(up) => return Ok(up),
            Err(e)
                if matches!(
                    e.code(),
                    "ssh_unreachable" | "start_timeout" | "restore_stalled"
                ) =>
            {
                member = pool
                    .store
                    .load()?
                    .project(&pool.project)
                    .and_then(|p| p.find(&member.name))
                    .cloned()
                    .ok_or_else(|| AppError::new("member_failed", e.to_string()))?;
                let Some(id) = &member.id else { return Err(e) };
                super::lease::mark_broken(&pool.store, &pool.project, id, e.message())?;
                e.to_string()
            }
            Err(e) => return Err(e),
        }
    };
    let Some(id) = member.id.clone() else {
        return run(pool, member, fresh, options);
    };
    let sandbox = get(pool.provider, &id)?;
    let preserve = if member.live_leases().any(|l| l.pid != std::process::id()) {
        Some("another lease is live".to_string())
    } else {
        pool.start_gate()
            .refused_today()
            .map(|e| format!("a replacement cannot start today ({e})"))
    };
    if let Some(why) = preserve {
        pool.out.note(format_args!(
            "{} is broken ({reason}), but {why}; keeping it",
            member.name
        ));
        return run(pool, member, fresh, options);
    }
    if !member.recovery.force_stopping
        && let Some(seconds) =
            restart_snapshot(&sandbox, member.recovery.restarted_at, system::now_ms())
    {
        step(
            pool,
            &member,
            "force-stopping for snapshot recovery",
            Duration::from_secs(180),
        )?;
        pool.out.note(format_args!("{} is stuck ({reason}); force-stopping to resume from the last snapshot. Writes since that snapshot will be lost", member.name));
        SshTarget::for_sandbox(pool.paths, &id).close_master();
        // Record intent before the POST: its reply or this process can be
        // lost after boat has already discarded the current machine.
        member = pool.modify(&member, |m| {
            m.recovery.restarted_at = Some(system::now_ms());
            m.recovery.snapshot_seconds = Some(seconds);
            m.recovery.force_stopping = true;
            m.recovery.restoring = true;
            m.sync = SyncState::default();
        })?;
        match pool.provider.force_stop(&id) {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Conflict
                        | ErrorKind::InvalidRequest
                        | ErrorKind::NotRunning
                        | ErrorKind::NotFound
                ) =>
            {
                member = pool.modify(&member, |m| m.recovery.force_stopping = false)?;
                pool.out
                    .note(format_args!("force-stop unavailable for {id}: {e}"));
            }
            Err(e) => {
                // A provider incident or key-scope refusal cannot justify
                // escalation to replacement on the next invocation.
                pool.modify(&member, |m| {
                    m.recovery.force_stopping = false;
                    m.recovery.restarted_at = None;
                })?;
                return Err(e.into());
            }
        }
    }
    if member.recovery.force_stopping || sandbox.state == SandboxState::Stopped {
        let since = member.recovery.restarted_at.unwrap_or_else(system::now_ms);
        step(
            pool,
            &member,
            "waiting for stopped snapshot",
            Duration::from_secs(180),
        )?;
        loop {
            let current = get(pool.provider, &id)?;
            if current.state == SandboxState::Stopped {
                let restarted = member.recovery.force_stopping;
                member = pool.modify(&member, |m| {
                    m.broken = None;
                    m.recovery.force_stopping = false;
                })?;
                match run(pool, member.clone(), false, options) {
                    Ok(up) => {
                        if restarted {
                            pool.recovery.set(Some("restarted"));
                        }
                        return Ok(up);
                    }
                    Err(e)
                        if matches!(
                            e.code(),
                            "ssh_unreachable"
                                | "member_failed"
                                | "start_timeout"
                                | "restore_stalled"
                                | "data_expired"
                                | "conflict"
                        ) =>
                    {
                        pool.out
                            .note(format_args!("snapshot recovery of {id} failed: {e}"));
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            if matches!(current.state, SandboxState::Error | SandboxState::Gone)
                || system::now_ms().saturating_sub(since) >= 180_000
            {
                pool.out.note(format_args!(
                    "waiting for stopped snapshot failed: {}",
                    current.condition()
                ));
                break;
            }
            std::thread::sleep(wait::poll_interval());
        }
    }
    if let Some(error) = pool.start_gate().refused_today() {
        return Err(error);
    }
    step(pool, &member, "replacing sandbox", Duration::from_secs(120))?;
    pool.out.note(format_args!(
        "{} ({id}) is broken: {reason}; deleting it and creating a new sandbox in its place",
        member.name
    ));
    discard(pool, &member.name, &id)?;
    pool.recovery.set(Some("replaced"));
    let key = system::uuid_v4()?;
    member = pool.modify(&member, |m| {
        m.id = None;
        m.create_key = key;
        m.sync = SyncState::default();
        m.created_at = system::now_ms();
        m.broken = None;
        m.recovery = Default::default();
    })?;
    run(pool, member, true, options)
}

fn seconds_env(key: &str, fallback: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback),
    )
}

pub(super) fn wait_restore(
    pool: &Pool<'_>,
    member: &Member,
    mut sandbox: Sandbox,
    ttl: u64,
) -> Result<(Sandbox, Duration), AppError> {
    if sandbox.hydration.is_complete() {
        return Ok((sandbox, Duration::ZERO));
    }
    let timeout = seconds_env("BOATPARTY_HYDRATION_TIMEOUT_SECONDS", 1800);
    let stall = seconds_env(super::lease::STALL_LIMIT_ENV, 600);
    step(pool, member, "restoring files", timeout)?;
    let began = Instant::now();
    let mut changed = began;
    let mut last_report = None;
    let mut last_extension = None;
    let mut progress = None;
    loop {
        if sandbox.state != SandboxState::Running {
            return Err(AppError::new(
                "not_running",
                format!("waiting for restore: {}", sandbox.condition()),
            ));
        }
        if sandbox.hydration.is_complete() {
            return Ok((sandbox, began.elapsed()));
        }
        let percent = match sandbox.hydration {
            Hydration::InProgress { percent } => percent,
            _ => None,
        };
        let observed = (sandbox.restore_entries, percent);
        if progress != Some(observed) {
            progress = Some(observed);
            changed = Instant::now()
        }
        if last_report.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(60)) {
            let progress = sandbox
                .restore_entries
                .map(|(n, m)| format!("{n} of {m} entries"))
                .or_else(|| percent.map(|n| format!("{n}%")))
                .unwrap_or_else(|| "progress unknown".into());
            pool.out.note(format_args!(
                "{} restoring: {progress}, {}s elapsed",
                member.name,
                began.elapsed().as_secs()
            ));
            last_report = Some(Instant::now());
        }
        if began.elapsed() >= timeout || changed.elapsed() >= stall {
            return Err(AppError::new(
                "restore_stalled",
                format!(
                    "waiting for restore timed out after {}s: {}",
                    began.elapsed().as_secs(),
                    sandbox.condition()
                ),
            ));
        }
        if last_extension
            .is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs((ttl / 2).clamp(1, 60)))
        {
            pool.provider.set_ttl(&sandbox.id, ttl)?;
            last_extension = Some(Instant::now());
        }
        std::thread::sleep(wait::poll_interval());
        sandbox = get(pool.provider, &sandbox.id)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_completed_restore_and_snapshot_outside_cooldown_can_restart() {
        let now = 5 * COOLDOWN_MS;
        for (complete, snapshot, last, expected) in [
            (true, Some(21), None, Some(21)),
            (true, None, None, None),
            (false, Some(21), None, None),
            (true, Some(21), Some(now - 1), None),
            (true, Some(21), Some(now - COOLDOWN_MS), Some(21)),
        ] {
            let mut s = Sandbox::gone("bx_test");
            s.state = SandboxState::Running;
            s.hydration = if complete {
                Hydration::Complete
            } else {
                Hydration::Unknown
            };
            s.snapshot_seconds = snapshot;
            assert_eq!(restart_snapshot(&s, last, now), expected);
        }
    }
}
