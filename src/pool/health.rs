//! Tunnel-dependent commands alone may reboot a degraded machine.
use super::{recovery::COOLDOWN_MS, wait};
use crate::{
    Context,
    error::AppError,
    provider::{Provider, SandboxState},
    state::{BringingUp, Member, Store},
    system,
    transport::ssh::SshTarget,
};
use std::time::{Duration, Instant};

pub fn ensure_tunnel(
    ctx: &Context,
    project: &str,
    member: &Member,
    target: &SshTarget,
    provider: &dyn Provider,
) -> Result<(), AppError> {
    let id = &target.alias;
    let before = provider.get(id)?;
    if before.degraded_since.is_none() {
        return Ok(());
    }
    let store = Store::new(&ctx.paths);
    let pid = std::process::id();
    store.update(|state| {
        let m = state
            .project_mut(project)
            .find_mut_by_key(&member.create_key)
            .filter(|m| m.id.as_ref() == Some(id))
            .ok_or_else(|| AppError::new("member_failed", "member changed before tunnel repair"))?;
        if m.live_leases().any(|l| l.pid != pid) || m.bring_up_claimed_by_other() {
            return Err(AppError::new(
                "tunnel_degraded",
                format!("{id} tunnel is degraded; reboot must wait until other commands finish"),
            ));
        }
        let now = system::now_ms();
        if m.recovery
            .rebooted_at
            .is_some_and(|at| now.saturating_sub(at) < COOLDOWN_MS)
        {
            return Err(AppError::new(
                "tunnel_degraded",
                format!("{id} tunnel is degraded after a reboot within the last 30 minutes"),
            ));
        }
        m.bringing_up = Some(BringingUp {
            step: "repairing provider tunnel".into(),
            limit_seconds: 360,
            ..BringingUp::new(pid, now)
        });
        m.recovery.rebooted_at = Some(now);
        Ok(())
    })?;
    ctx.out.note(format_args!(
        "{}; rebooting over SSH to restore the tunnel",
        before.condition()
    ));
    let result = (|| {
        let (status, err) =
            target.run_quiet("sudo -n systemd-run --on-active=5 /bin/systemctl reboot")?;
        if status != 0 {
            return Err(AppError::new(
                "tunnel_degraded",
                format!("scheduling reboot of {id} failed: {err}"),
            ));
        }
        target.close_master();
        let began = Instant::now();
        let deadline = wait::Deadline::after(Duration::from_secs(300));
        while deadline.tick() {
            let next = match provider.get(id) {
                Ok(next) => next,
                Err(e) if wait::transient(e.kind()) => continue,
                Err(e) => return Err(e.into()),
            };
            if next.state == SandboxState::Running
                && next.health.as_deref() == Some("ok")
                && (next.last_reboot != before.last_reboot
                    || began.elapsed() >= Duration::from_secs(45))
            {
                super::ensure_connected(&ctx.paths, provider, ctx.out, id, true)?;
                return Ok(());
            }
        }
        Err(AppError::new(
            "tunnel_degraded",
            format!("{id} tunnel did not recover within five minutes of reboot"),
        ))
    })();
    store.update(|state| {
        if let Some(m) = state
            .project_mut(project)
            .find_mut_by_key(&member.create_key)
        {
            m.bringing_up = None;
        }
        Ok(())
    })?;
    result
}
