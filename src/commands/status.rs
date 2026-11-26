//! `boatparty status`: this project's members, as recorded and as the
//! provider reports them.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use super::WorkspaceContext;
use crate::Context;
use crate::error::AppError;
use crate::paths::Paths;
use crate::pool::{LABEL_PROJECT, wait};
use crate::provider::{Sandbox, SandboxState};
use crate::state::Member;
use crate::transport::ssh::SshTarget;

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let project = pool.project.clone();
    let workspace = wctx.workspace.key();
    let members = pool
        .store
        .load()?
        .project(&project)
        .map(|pool| pool.members.clone())
        .unwrap_or_default();

    let listed = wait::retry(Duration::from_secs(60), wait::transient, || {
        wctx.provider.list_by_label(LABEL_PROJECT, &project)
    })?;
    let mut by_id: BTreeMap<String, Sandbox> = listed
        .into_iter()
        .map(|sandbox| (sandbox.id.clone(), sandbox))
        .collect();
    let mut rows = Vec::new();
    for member in &members {
        let sandbox = match &member.id {
            Some(id) => match by_id.remove(id) {
                Some(sandbox) => Some(sandbox),
                // Not listed: ask directly, which also tells a deleted one apart.
                None => Some(wait::retry(
                    Duration::from_secs(60),
                    wait::transient,
                    || wctx.provider.get(id),
                )?),
            },
            None => None,
        };
        rows.push((member, sandbox));
    }
    let untracked: Vec<Sandbox> = by_id.into_values().collect();
    let (base, base_note) = super::base::summary(ctx, &wctx.project, &wctx.workspace.root);

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": project,
            "workspace": workspace,
            "members": rows
                .iter()
                .map(|(member, sandbox)| member_json(&ctx.paths, member, sandbox.as_ref(), &workspace))
                .collect::<Vec<_>>(),
            "untracked": untracked.iter().map(|sandbox| json!({
                "id": sandbox.id,
                "name": sandbox.name,
                "state": sandbox.state.as_str(),
            })).collect::<Vec<_>>(),
            "base": base,
        }));
        return Ok(0);
    }

    ctx.out.text(format_args!("project {project}"));
    if rows.is_empty() {
        ctx.out
            .text("no sandboxes; start one with `boatparty start`");
    }
    for (member, sandbox) in &rows {
        let mine = member.affinity.as_deref() == Some(workspace.as_str());
        let state = state_word(member, sandbox.as_ref());
        let mut line = format!(
            "{} {:<20} {:<12} {:<9}",
            if mine { "*" } else { " " },
            member.name,
            member.id.as_deref().unwrap_or("-"),
            state
        );
        if let Some(sandbox) = sandbox
            && sandbox.state == SandboxState::Running
        {
            if sandbox.degraded_since.is_some() {
                line.push_str(" tunnel degraded");
            }
            line.push_str(&format!(" files {}", sandbox.hydration.as_str()));
            line.push_str(&format!(
                " auto-stop {}",
                sandbox.auto_stop_at.as_deref().unwrap_or("off")
            ));
        }
        if let Some(affinity) = &member.affinity {
            line.push_str(&format!(" for {affinity}"));
        }
        ctx.out.text(line.trim_end());
        if let Some(broken) = &member.broken {
            ctx.out.text(format_args!(
                "  broken: {}; snapshot recovery is attempted before replacement",
                broken.reason
            ));
        }
    }
    for sandbox in &untracked {
        ctx.out.text(format_args!(
            "  untracked {} {} {}",
            sandbox.name.as_deref().unwrap_or("-"),
            sandbox.id,
            sandbox.state
        ));
    }
    if let Some(note) = base_note {
        ctx.out.text(note);
    }
    Ok(0)
}

fn state_word(member: &Member, sandbox: Option<&Sandbox>) -> &'static str {
    match (member.id.as_ref(), sandbox) {
        (None, _) => "pending",
        (Some(_), Some(sandbox)) => sandbox.state.as_str(),
        (Some(_), None) => "unknown",
    }
}

/// One member as `--json` shows it.
pub fn member_json(
    paths: &Paths,
    member: &Member,
    sandbox: Option<&Sandbox>,
    workspace: &str,
) -> Value {
    let ssh = member.id.as_deref().map(|id| {
        let target = SshTarget::for_sandbox(paths, id);
        json!({
            "host_alias": target.alias,
            "config": target.config.display().to_string(),
            "known_hosts": target.known_hosts.display().to_string(),
            "configured": target.config.exists(),
        })
    });
    json!({
        "name": member.name,
        "id": member.id,
        "role": member.role.as_str(),
        "state": state_word(member, sandbox),
        "health": sandbox.and_then(|s| s.health.as_ref()),
        "health_reason": sandbox.and_then(|s| s.health_reason.as_ref()),
        "restarted_from": member.recovery.snapshot_seconds,
        "hydration": sandbox.map(|sandbox| sandbox.hydration.as_str()),
        "size": sandbox.and_then(|sandbox| sandbox.size).map(|size| size.as_str()),
        "affinity": member.affinity,
        "this_workspace": member.affinity.as_deref() == Some(workspace),
        "auto_stop_at": sandbox.and_then(|sandbox| sandbox.auto_stop_at.clone()),
        "leases": member.live_leases().map(|lease| json!({
            "pid": lease.pid,
            "kind": lease.kind,
            "label": lease.label,
            "started_at": lease.started_at,
        })).collect::<Vec<_>>(),
        "bringing_up": member.bringing_up,
        "ssh": ssh,
        "created_at": member.created_at,
        "last_used_at": member.last_used_at,
        "broken": member.broken.as_ref().map(|broken| broken.reason.clone()),
    })
}
