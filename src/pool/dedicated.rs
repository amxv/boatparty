//! Exclusive members without workspace affinity.
use super::acquire::{Facts, count_running, has_room};
use super::bringup::claimed;
use super::{BringUp, Pool, Up, new_member};
use crate::state::{BringingUp, Lease, LeaseKind, Member, Role};
use crate::transport::ssh::SshTarget;
use crate::{error::AppError, system};

/// Bring the project's builder up for a base build, creating its record
/// (role `builder`, no affinity) on first use. This process holds it with
/// a lease until the build releases it; another live build is refused.
pub fn bring_up_builder(pool: &Pool<'_>, options: BringUp) -> Result<Up, AppError> {
    bring_up_dedicated(pool, options, Role::Builder)
}

pub fn bring_up_gate(pool: &Pool<'_>, options: BringUp) -> Result<Up, AppError> {
    bring_up_dedicated(pool, options, Role::Gate)
}

fn bring_up_dedicated(pool: &Pool<'_>, options: BringUp, role: Role) -> Result<Up, AppError> {
    let project = pool.project.clone();
    let name = format!("bp-{project}-{}", role.as_str());
    let caps = pool.limits.max_running;
    let listed = if caps.any() {
        pool.running_members()
    } else {
        super::Listed::default()
    };
    let account_running = caps
        .account
        .and_then(|_| crate::starts::refresh(&pool.store, pool.provider))
        .map(|cache| cache.running);
    let facts = Facts {
        running: &listed.running,
        exists: &super::workspace_exists,
        active: listed.ok.then_some(&listed.active),
        account_running,
    };
    let key = system::uuid_v4()?;
    let now = system::now_ms();
    let pid = std::process::id();
    let claim = pool.store.update(|state| {
        let slot = state.project_mut(&project);
        let count = count_running(slot, &facts);
        let index = match slot.members.iter().position(|member| member.role == role) {
            Some(index) => index,
            None => {
                if role == Role::Gate {
                    let occupied = slot
                        .members
                        .iter()
                        .filter(|m| m.role != Role::Builder)
                        .count();
                    if occupied >= pool.limits.max_sandboxes as usize {
                        return Err(AppError::new("pool_exhausted", format!(
                            "the gate needs its own member, but {project} already uses {occupied} of {} sandbox slots",
                            pool.limits.max_sandboxes
                        )).with_hint("raise max_sandboxes or remove an unused member"));
                    }
                }
                let mut member = new_member(name.clone(), key.clone(), None, now);
                member.role = role;
                slot.members.push(member);
                slot.members.len() - 1
            }
        };
        let member = &mut slot.members[index];
        let fresh = member.create_key == key;
        if member.busy() {
            return Ok(Err(member.clone()));
        }
        if !member
            .id
            .as_ref()
            .is_some_and(|id| listed.active.contains(id))
        {
            if (caps.project.is_some() && !listed.ok)
                || (caps.account.is_some() && account_running.is_none())
            {
                return Err(AppError::new(
                    "provider_error",
                    "cannot verify max_running for the dedicated member",
                ));
            }
            if !has_room(caps, count) {
                return Err(pool.running_full(count));
            }
        }
        member
            .leases
            .retain(|lease| lease.pid != pid && system::pid_alive(lease.pid));
        member.leases.push(Lease {
            pid,
            kind: if role == Role::Gate {
                LeaseKind::Run
            } else {
                LeaseKind::Session
            },
            label: if role == Role::Gate {
                "gate"
            } else {
                "base build"
            }
            .to_string(),
            started_at: now,
            workspace: None,
        });
        member.bringing_up = Some(BringingUp::new(pid, now));
        Ok(Ok((member.clone(), fresh)))
    })?;
    let (member, fresh) = claim.map_err(|busy| {
        AppError::new(
            if role == Role::Gate {
                "gate_busy"
            } else {
                "base_busy"
            },
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
        .with_hint("wait for that command to finish")
    })?;
    let result = claimed(pool, member, fresh, options);
    if result.is_err() {
        release(pool, role);
    }
    result
}

/// Drop this process's lease on the builder.
pub fn release_builder(pool: &Pool<'_>) {
    release(pool, Role::Builder);
}

fn release(pool: &Pool<'_>, role: Role) {
    let pid = std::process::id();
    let project = pool.project.clone();
    let _ = pool.store.update(|state| {
        for member in &mut state.project_mut(&project).members {
            if member.role == role {
                member.leases.retain(|lease| lease.pid != pid);
            }
        }
        Ok(())
    });
}

/// Probe SSH outside the lock, then lease exactly the member that answered.
pub fn warm_gate(pool: &Pool<'_>) -> Result<Option<(Member, SshTarget)>, AppError> {
    let state = pool.store.load()?;
    let Some(member) = state
        .project(&pool.project)
        .and_then(|slot| slot.members.iter().find(|m| m.role == Role::Gate))
    else {
        return Ok(None);
    };
    if member.busy() {
        return Err(AppError::new(
            "gate_busy",
            format!("{} is in use", member.name),
        ));
    }
    let Some(id) = &member.id else {
        return Ok(None);
    };
    let target = SshTarget::for_sandbox(pool.paths, id);
    if member.broken.is_some() || member.recovery.restoring || !target.reachable() {
        return Ok(None);
    }
    pool.store.update(|state| {
        let Some(slot) = state
            .project_mut(&pool.project)
            .find_mut_by_key(&member.create_key)
        else {
            return Ok(None);
        };
        if slot.id != member.id || slot.broken.is_some() || slot.recovery.restoring {
            return Ok(None);
        }
        if slot.busy() {
            return Err(AppError::new(
                "gate_busy",
                format!("{} is in use", slot.name),
            ));
        }
        slot.leases.clear();
        slot.leases.push(Lease {
            pid: std::process::id(),
            kind: LeaseKind::Run,
            label: "gate".into(),
            started_at: system::now_ms(),
            workspace: None,
        });
        Ok(Some((slot.clone(), target)))
    })
}
