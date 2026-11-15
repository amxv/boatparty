//! `boatparty load`: make the sandbox's checkout identical to this working
//! tree.
//!
//! When the workspace's member has a live SSH master no provider call is
//! made before the sync. A `session` lease is held while it runs. A warm connection that turns out to be stale (the member stopped
//! behind a lingering master) is replaced once by a full bring-up; a sync is
//! idempotent, so repeating it is safe.

use std::time::Instant;

use serde_json::json;

use super::WorkspaceContext;
use crate::Context;
use crate::error::AppError;
use crate::pool::lease;
use crate::pool::{LeaseRequest, remote_checkout, warm_member};
use crate::project::Workspace;
use crate::runner::RunnerSpec;
use crate::state::{LeaseKind, Member, Store, SyncState};
use crate::sync::pack::usable_known;
use crate::sync::{
    self, History, LocalRef, MirrorOutcome, MirrorRequest, PackRoute, SeedConfig, SeedRoute,
    Snapshot, WorktreeInfo,
};
use crate::system;
use crate::transport::ssh::SshTarget;

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let started = Instant::now();
    let workspace = Workspace::current()?;
    let project = workspace.load_project(&ctx.paths)?;
    let name = project.name();
    // Local work first: a repository without a commit fails before any
    // provider call.
    let info = WorktreeInfo::read(&workspace.root)?;
    let history = project.history();
    let snapshot = sync::snapshot(&info)?;
    let (refs, digest) = sync::mirrored_refs(&info, history)?;
    let seed = project.seed_config();
    let spec = RunnerSpec::new(project.config());

    // A `session` lease keeps the member from being handed to another
    // workspace or stopped while the sync runs.
    let store = Store::new(&ctx.paths);
    let key = workspace.key();
    let (mut member, mut target, warm) = match warm_member(&ctx.paths, name, &key)? {
        Some((member, target))
            if lease::register(&store, name, &key, &member, LeaseKind::Session, "load")? =>
        {
            (member, target, true)
        }
        _ => {
            let (member, target) = bring_up(ctx)?;
            (member, target, false)
        }
    };
    let local = Local {
        project: name,
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        digest: &digest,
        history,
        seed: &seed,
        spec: &spec,
    };
    let send = |member: &Member, target: &SshTarget| send(ctx, &local, member, target);
    let outcome = match send(&member, &target) {
        Err(error) if warm && error.code() == "connection" => {
            ctx.out.note(format_args!(
                "the connection to {} was stale; reconnecting",
                member.name
            ));
            target.close_master();
            match bring_up(ctx) {
                Ok(up) => {
                    (member, target) = up;
                    send(&member, &target)
                }
                Err(error) => Err(error),
            }
        }
        result => result,
    };
    // As after any command: the last leaver hands the auto-stop deadline to
    // the detached idle helper, so the caller makes no provider call.
    let id = member.id.clone().unwrap_or_default();
    let released = lease::finish(
        &store,
        name,
        &id,
        |_| {},
        || {
            if let Err(error) = lease::spawn_idle_helper(name, &id) {
                ctx.out.note(format_args!("warning: {error}"));
            }
        },
    );
    if let Err(error) = released {
        ctx.out.note(format_args!(
            "warning: could not release the lease: {error}"
        ));
    }
    let outcome = outcome?;
    let member = record(ctx, &local, &member, &outcome)?;
    let seconds = started.elapsed().as_secs_f64();
    report(ctx, &local, &workspace, &member, &outcome, seconds);
    Ok(0)
}

/// What this sync sends.
struct Local<'a> {
    project: &'a str,
    info: &'a WorktreeInfo,
    snapshot: &'a Snapshot,
    refs: &'a [LocalRef],
    digest: &'a str,
    history: History,
    seed: &'a SeedConfig,
    /// Named by the sync; a bare load applies none of it.
    spec: &'a RunnerSpec,
}

fn bring_up(ctx: &Context) -> Result<(Member, SshTarget), AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let up = wctx.pool(ctx).bring_up(
        wctx.bring_up_options(false),
        Some(&LeaseRequest::new(LeaseKind::Session, "load")),
    )?;
    Ok((up.member, up.target))
}

fn send(
    ctx: &Context,
    local: &Local<'_>,
    member: &Member,
    target: &SshTarget,
) -> Result<MirrorOutcome, AppError> {
    let request = MirrorRequest {
        project: local.project,
        info: local.info,
        snapshot: local.snapshot,
        refs: local.refs,
        send_refs: member.sync.refs_digest.as_deref() != Some(local.digest),
        known: usable_known(local.history, &member.sync.mode, &member.sync.known),
        history: local.history,
        seed: Some(local.seed),
    };
    let host = super::runner_host(ctx, target, member, local.spec.clone());
    sync::mirror(&host, &request, &mut std::io::stderr())
}

/// Replace the member's sync record with what the sandbox now holds.
fn record(
    ctx: &Context,
    local: &Local<'_>,
    member: &Member,
    outcome: &MirrorOutcome,
) -> Result<Member, AppError> {
    let create_key = member.create_key.clone();
    let known = outcome.known.clone();
    let digest = local.digest.to_string();
    let mode = local.history.as_str().to_string();
    let updated = Store::new(&ctx.paths).update(|state| {
        let pool = state.project_mut(local.project);
        let Some(slot) = pool.find_mut_by_key(&create_key) else {
            return Ok(None);
        };
        slot.sync = SyncState {
            mode,
            known,
            refs_digest: Some(digest),
        };
        slot.last_used_at = system::now_ms();
        Ok(Some(slot.clone()))
    })?;
    Ok(updated.unwrap_or_else(|| member.clone()))
}

fn report(
    ctx: &Context,
    local: &Local<'_>,
    workspace: &Workspace,
    member: &Member,
    outcome: &MirrorOutcome,
    seconds: f64,
) {
    let (project, info) = (local.project, local.info);
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
            "wip": local.snapshot.wip,
            "tree": outcome.tree,
            "history": local.history.as_str(),
            "pack_bytes": outcome.pack_bytes,
            "pack_route": outcome.pack_route.as_str(),
            "upload_fallback": outcome.upload_fallback,
            "seed_route": outcome.seed.route.as_str(),
            "seed_fallback": outcome.seed.fallback,
            "seed_skipped": outcome.seed.skipped,
            "seed_seconds": outcome.seed.seconds(),
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
    let mut route = match outcome.pack_route {
        PackRoute::SignedUrl => " by signed upload".to_string(),
        _ => String::new(),
    };
    if outcome.seed.route == SeedRoute::Github {
        route.push_str(&format!(
            " after seeding from GitHub in {:.1}s",
            outcome.seed.seconds()
        ));
    }
    ctx.out.text(format_args!(
        "loaded {at} into {} ({id}) {}: sent {}{route}, {seconds:.1}s",
        member.name,
        remote_checkout(project),
        size(outcome.pack_bytes),
    ));
    if let Some(reason) = &outcome.upload_fallback {
        ctx.out.note(format_args!(
            "the signed upload failed ({reason}); the pack went over SSH instead"
        ));
    }
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
