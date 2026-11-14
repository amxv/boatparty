//! `boatparty load`: make the sandbox's checkout identical to this working
//! tree.
//!
//! When the workspace's member has a live SSH master no provider call is
//! made. A warm connection that turns out to be stale (the member stopped
//! behind a lingering master) is replaced once by a full bring-up; a sync is
//! idempotent, so repeating it is safe.

use std::time::Instant;

use serde_json::json;

use super::WorkspaceContext;
use crate::Context;
use crate::error::AppError;
use crate::pool::{remote_checkout, warm_member};
use crate::project::Workspace;
use crate::runner::SshRunner;
use crate::state::{Member, Store, SyncState};
use crate::sync::{
    self, LocalRef, MODE_FULL, MirrorOutcome, MirrorRequest, Snapshot, WorktreeInfo,
};
use crate::system;
use crate::transport::ssh::SshTarget;

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let started = Instant::now();
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let name = project.name();
    // Local work first: a repository without a commit fails before any
    // provider call.
    let info = WorktreeInfo::read(&workspace.root)?;
    let snapshot = sync::snapshot(&info)?;
    let refs = sync::local_refs(&info)?;
    let digest = sync::refs_digest(&refs);

    let (mut member, mut target, warm) = match warm_member(&ctx.paths, name, &workspace.key())? {
        Some((member, target)) => (member, target, true),
        None => {
            let (member, target) = bring_up(ctx)?;
            (member, target, false)
        }
    };
    let send = |member: &Member, target: &SshTarget| {
        send(name, &info, &snapshot, &refs, &digest, member, target)
    };
    let outcome = match send(&member, &target) {
        Err(error) if warm && error.code() == "connection" => {
            ctx.out.note(format_args!(
                "the connection to {} was stale; reconnecting",
                member.name
            ));
            target.close_master();
            (member, target) = bring_up(ctx)?;
            send(&member, &target)?
        }
        result => result?,
    };
    let member = record(ctx, name, &member, &outcome, &digest)?;
    let seconds = started.elapsed().as_secs_f64();
    report(
        ctx, name, &workspace, &info, &snapshot, &member, &outcome, seconds,
    );
    Ok(0)
}

fn bring_up(ctx: &Context) -> Result<(Member, SshTarget), AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let up = wctx.pool(ctx).bring_up(wctx.bring_up_options(false))?;
    Ok((up.member, up.target))
}

fn send(
    project: &str,
    info: &WorktreeInfo,
    snapshot: &Snapshot,
    refs: &[LocalRef],
    digest: &str,
    member: &Member,
    target: &SshTarget,
) -> Result<MirrorOutcome, AppError> {
    let request = MirrorRequest {
        project,
        info,
        snapshot,
        refs,
        send_refs: member.sync.refs_digest.as_deref() != Some(digest),
        known: &member.sync.known,
    };
    let host = SshRunner {
        target: target.clone(),
    };
    sync::mirror(&host, &request, &mut std::io::stderr())
}

/// Replace the member's sync record with what the sandbox now holds.
fn record(
    ctx: &Context,
    project: &str,
    member: &Member,
    outcome: &MirrorOutcome,
    digest: &str,
) -> Result<Member, AppError> {
    let create_key = member.create_key.clone();
    let known = outcome.known.clone();
    let digest = digest.to_string();
    let updated = Store::new(&ctx.paths).update(|state| {
        let pool = state.project_mut(project);
        let Some(slot) = pool.find_mut_by_key(&create_key) else {
            return Ok(None);
        };
        slot.sync = SyncState {
            mode: MODE_FULL.to_string(),
            known,
            refs_digest: Some(digest),
        };
        slot.last_used_at = system::now_ms();
        Ok(Some(slot.clone()))
    })?;
    Ok(updated.unwrap_or_else(|| member.clone()))
}

#[allow(clippy::too_many_arguments)]
fn report(
    ctx: &Context,
    project: &str,
    workspace: &Workspace,
    info: &WorktreeInfo,
    snapshot: &Snapshot,
    member: &Member,
    outcome: &MirrorOutcome,
    seconds: f64,
) {
    let id = member.id.clone().unwrap_or_default();
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": project,
            "workspace": workspace.key(),
            "member": { "name": member.name, "id": id },
            "checkout": remote_checkout(project),
            "head": info.head,
            "branch": info.branch,
            "wip": snapshot.wip,
            "tree": outcome.tree,
            "history": MODE_FULL,
            "pack_bytes": outcome.pack_bytes,
            "full_retry": outcome.full_retry,
            "runner_installed": outcome.installed,
            "seconds": (seconds * 1000.0).round() / 1000.0,
        }));
        return;
    }
    let at = match &info.branch {
        Some(branch) => format!("{branch} at {}", short(&info.head)),
        None => format!("detached at {}", short(&info.head)),
    };
    ctx.out.text(format_args!(
        "loaded {at} into {} ({id}) {}: sent {}, {seconds:.1}s",
        member.name,
        remote_checkout(project),
        size(outcome.pack_bytes),
    ));
    if outcome.full_retry {
        ctx.out
            .note("the sandbox lacked objects it was thought to have, so a full pack was sent");
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

fn size(bytes: u64) -> String {
    match bytes {
        0 => "no new objects".to_string(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    }
}
