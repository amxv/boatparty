//! `boatparty pool`: every project's members, their state, affinity, leases
//! and last use; `--refresh` first reconciles the records with the
//! provider's labels. `boatparty pool prune` deletes stopped members that
//! no workspace needs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

use serde_json::{Value, json};

use crate::Context;
use crate::cli::{PoolAction, PoolArgs, PruneArgs};
use crate::error::AppError;
use crate::pool::reconcile::{self, Changes, prunable};
use crate::pool::remove::delete_sandbox;
use crate::pool::{PoolLimits, workspace_exists};
use crate::provider::{Provider, Sandbox, SandboxState};
use crate::state::{Member, Store};
use crate::system;

pub fn run(ctx: &Context, args: &PoolArgs) -> Result<u8, AppError> {
    match &args.action {
        Some(PoolAction::Prune(prune_args)) => prune(ctx, prune_args),
        None => show(ctx, args),
    }
}

/// Every project this machine knows: registered or holding records.
fn project_names(
    ctx: &Context,
    config: &crate::account::config::AccountConfig,
    only: Option<&str>,
) -> Result<Vec<String>, AppError> {
    let state = Store::new(&ctx.paths).load()?;
    let mut names: BTreeSet<String> = config.projects.keys().cloned().collect();
    names.extend(state.projects.keys().cloned());
    if let Some(only) = only {
        if !names.contains(only) {
            return Err(AppError::new(
                "no_member",
                format!("this machine knows no project named {only}"),
            ));
        }
        return Ok(vec![only.to_string()]);
    }
    Ok(names.into_iter().collect())
}

/// The provider when a key is stored, for read-only listings.
fn optional_provider(
    ctx: &Context,
    config: &crate::account::config::AccountConfig,
) -> Option<Box<dyn Provider>> {
    let (key, _) = crate::account::credentials::load(&ctx.paths).ok()??;
    crate::provider::connect(config.provider.api_url.as_deref(), key).ok()
}

struct ProjectView {
    name: String,
    limits: PoolLimits,
    members: Vec<Member>,
    listed: Option<Vec<Sandbox>>,
    provider_error: Option<String>,
    changes: Option<Changes>,
}

fn show(ctx: &Context, args: &PoolArgs) -> Result<u8, AppError> {
    let account = super::load_account(ctx)?;
    let names = project_names(ctx, &account.config, args.project.as_deref())?;
    let provider = if args.refresh {
        Some(super::open_provider(ctx)?)
    } else {
        optional_provider(ctx, &account.config)
    };
    let store = Store::new(&ctx.paths);
    let mut views = Vec::new();
    for name in names {
        let mut view = ProjectView {
            limits: PoolLimits::for_project(&account.config, &name),
            name,
            members: Vec::new(),
            listed: None,
            provider_error: None,
            changes: None,
        };
        match provider.as_deref() {
            Some(provider) if args.refresh => {
                let (changes, listed) =
                    reconcile::reconcile(&ctx.paths, provider, ctx.out, &view.name)?;
                view.changes = Some(changes);
                view.listed = Some(listed);
            }
            Some(provider) => match reconcile::list(provider, &view.name) {
                Ok(listed) => view.listed = Some(listed),
                Err(error) => view.provider_error = Some(error.message().to_string()),
            },
            None => view.provider_error = Some("no API key is stored".to_string()),
        }
        view.members = store
            .load()?
            .project(&view.name)
            .map(|pool| pool.members.clone())
            .unwrap_or_default();
        views.push(view);
    }

    let now = system::now_ms();
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "refreshed": args.refresh,
            "projects": views.iter().map(project_json).collect::<Vec<_>>(),
        }));
        return Ok(0);
    }
    if views.is_empty() {
        ctx.out
            .text("no projects yet; run `boatparty init` in a Git repository");
    }
    for view in &views {
        render(ctx, view, now);
    }
    Ok(0)
}

fn states_of(view: &ProjectView) -> BTreeMap<String, SandboxState> {
    view.listed
        .as_deref()
        .map(reconcile::states)
        .unwrap_or_default()
}

/// A member's state word: the provider's, or pending/unknown.
fn state_word(member: &Member, states: &BTreeMap<String, SandboxState>, listed: bool) -> String {
    match &member.id {
        None => "pending".into(),
        Some(id) => match states.get(id) {
            Some(state) => state.as_str().into(),
            // A listing that omits a recorded sandbox: it is gone.
            None if listed => SandboxState::Gone.as_str().into(),
            None => "unknown".into(),
        },
    }
}

fn untracked(view: &ProjectView) -> Vec<&Sandbox> {
    let known: BTreeSet<&str> = view
        .members
        .iter()
        .filter_map(|member| member.id.as_deref())
        .collect();
    view.listed
        .iter()
        .flatten()
        .filter(|sandbox| !known.contains(sandbox.id.as_str()))
        .collect()
}

fn member_json(member: &Member, state: String) -> Value {
    json!({
        "name": member.name,
        "id": member.id,
        "role": member.role.as_str(),
        "state": state,
        "affinity": member.affinity,
        "workspace_exists": member.affinity.as_deref().map(workspace_exists),
        "leases": member.live_leases().map(|lease| json!({
            "pid": lease.pid,
            "kind": lease.kind,
            "label": lease.label,
            "workspace": lease.workspace,
            "started_at": lease.started_at,
        })).collect::<Vec<_>>(),
        "bringing_up": member.bringing_up.as_ref()
            .filter(|claim| system::pid_alive(claim.pid))
            .map(|claim| json!(claim)),
        "created_at": member.created_at,
        "last_used_at": member.last_used_at,
        "last_run": member.last_run,
        "broken": member.broken.as_ref().map(|broken| broken.reason.clone()),
    })
}

fn project_json(view: &ProjectView) -> Value {
    let states = states_of(view);
    json!({
        "project": view.name,
        "max_sandboxes": view.limits.max_sandboxes,
        "steal_after_minutes": view.limits.steal_after_minutes,
        "members": view.members.iter()
            .map(|member| member_json(member, state_word(member, &states, view.listed.is_some())))
            .collect::<Vec<_>>(),
        "untracked": untracked(view).iter().map(|sandbox| json!({
            "id": sandbox.id,
            "name": sandbox.name,
            "state": sandbox.state.as_str(),
        })).collect::<Vec<_>>(),
        "provider_error": view.provider_error,
        "changes": view.changes,
    })
}

/// "3m ago" and friends, from milliseconds since the epoch.
pub fn ago(at: u64, now: u64) -> String {
    if at == 0 {
        return "never".into();
    }
    let seconds = now.saturating_sub(at) / 1000;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn render(ctx: &Context, view: &ProjectView, now: u64) {
    let pool_size = view
        .members
        .iter()
        .filter(|member| member.role == crate::state::Role::Pool)
        .count();
    ctx.out.text(format_args!(
        "project {}: {pool_size} of at most {} sandboxes",
        view.name, view.limits.max_sandboxes
    ));
    if let Some(error) = &view.provider_error {
        ctx.out
            .text(format_args!("  (provider state unavailable: {error})"));
    }
    let states = states_of(view);
    for member in &view.members {
        let owner = match member.affinity.as_deref() {
            None => "free".to_string(),
            Some(workspace) if workspace_exists(workspace) => workspace.to_string(),
            Some(workspace) => format!("{workspace} (gone)"),
        };
        let leases: Vec<String> = member
            .live_leases()
            .map(|lease| format!("{} {:?} pid {}", kind(lease.kind), lease.label, lease.pid))
            .collect();
        let mut line = format!(
            "  {:<20} {:<12} {:<9} {owner}",
            member.name,
            member.id.as_deref().unwrap_or("-"),
            state_word(member, &states, view.listed.is_some()),
        );
        if !leases.is_empty() {
            line.push_str(&format!("  in use: {}", leases.join(", ")));
        }
        if member.bringing_up.is_some() && member.bring_up_claimed_by_other() {
            line.push_str("  starting");
        }
        line.push_str(&format!("  used {}", ago(member.last_used_at, now)));
        ctx.out.text(line);
    }
    for sandbox in untracked(view) {
        ctx.out.text(format_args!(
            "  untracked {} {} {} (adopt it with `boatparty pool --refresh`)",
            sandbox.name.as_deref().unwrap_or("-"),
            sandbox.id,
            sandbox.state
        ));
    }
    if let Some(changes) = &view.changes {
        for (what, list) in [
            ("adopted", &changes.adopted),
            ("dropped (gone at the provider)", &changes.dropped),
            ("deleted (data expired)", &changes.expired),
        ] {
            if !list.is_empty() {
                let names: Vec<&str> = list.iter().map(|named| named.name.as_str()).collect();
                ctx.out.text(format_args!("  {what}: {}", names.join(", ")));
            }
        }
        for (named, workspace) in &changes.freed {
            ctx.out.text(format_args!(
                "  freed {} (its workspace {workspace} no longer exists)",
                named.name
            ));
        }
        for error in &changes.errors {
            ctx.out.text(format_args!("  error: {error}"));
        }
    }
}

fn kind(kind: crate::state::LeaseKind) -> &'static str {
    match kind {
        crate::state::LeaseKind::Run => "run",
        crate::state::LeaseKind::Serve => "serve",
        crate::state::LeaseKind::Session => "session",
    }
}

/// One member `prune` would delete.
struct Candidate {
    project: String,
    member: Member,
}

fn prune(ctx: &Context, args: &PruneArgs) -> Result<u8, AppError> {
    let account = super::load_account(ctx)?;
    let names = project_names(ctx, &account.config, args.project.as_deref())?;
    let provider = super::open_provider(ctx)?;
    let store = Store::new(&ctx.paths);
    let mut candidates = Vec::new();
    let mut listed_states = BTreeMap::new();
    for name in &names {
        let listed = reconcile::list(provider.as_ref(), name)?;
        let states = reconcile::states(&listed);
        let state = store.load()?;
        for member in state
            .project(name)
            .map(|pool| pool.members.as_slice())
            .unwrap_or(&[])
        {
            let current = member.id.as_ref().and_then(|id| states.get(id)).copied();
            if prunable(member, current, &workspace_exists) {
                candidates.push(Candidate {
                    project: name.clone(),
                    member: member.clone(),
                });
            }
        }
        listed_states.insert(name.clone(), states);
    }
    let described = |candidate: &Candidate| {
        json!({
            "project": candidate.project,
            "name": candidate.member.name,
            "id": candidate.member.id,
            "affinity": candidate.member.affinity,
        })
    };
    if candidates.is_empty() {
        if ctx.out.is_json() {
            ctx.out.document(&json!({
                "ok": true, "candidates": [], "deleted": [], "skipped": [], "aborted": false,
            }));
        } else {
            ctx.out.text("nothing to prune: no stopped sandbox is free");
        }
        return Ok(0);
    }
    ctx.out.note(format_args!(
        "{} stopped sandbox{} no workspace needs:",
        candidates.len(),
        if candidates.len() == 1 { "" } else { "es" }
    ));
    for candidate in &candidates {
        ctx.out.note(format_args!(
            "  {} {} ({}){}",
            candidate.project,
            candidate.member.name,
            candidate.member.id.as_deref().unwrap_or("-"),
            candidate
                .member
                .affinity
                .as_deref()
                .map(|workspace| format!(", workspace {workspace} is gone"))
                .unwrap_or_default()
        ));
    }
    if !args.yes && !confirm("delete them? [y/N] ")? {
        if ctx.out.is_json() {
            ctx.out.document(&json!({
                "ok": true,
                "candidates": candidates.iter().map(described).collect::<Vec<_>>(),
                "deleted": [], "skipped": [], "aborted": true,
            }));
        } else {
            ctx.out.text("nothing was deleted");
        }
        return Ok(0);
    }

    let mut deleted = Vec::new();
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    for candidate in &candidates {
        let project = &candidate.project;
        let id = candidate.member.id.clone().unwrap_or_default();
        let current = listed_states
            .get(project)
            .and_then(|states| states.get(&id))
            .copied();
        // Take the record out first, so no workspace can acquire the member
        // while it is being deleted; recheck it under the lock.
        let removed = store.update(|state| {
            let pool = state.project_mut(project);
            let still = pool
                .members
                .iter()
                .find(|member| member.create_key == candidate.member.create_key)
                .is_some_and(|member| prunable(member, current, &workspace_exists));
            Ok(still
                .then(|| pool.remove_by_key(&candidate.member.create_key))
                .flatten())
        })?;
        let Some(member) = removed else {
            skipped.push(described(candidate));
            continue;
        };
        match delete_sandbox(&ctx.paths, provider.as_ref(), ctx.out, &member.name, &id) {
            Ok(_) => deleted.push(described(candidate)),
            Err(error) => {
                errors.push(format!("{}: {error}", member.name));
                store.update(|state| {
                    state.project_mut(project).members.push(member.clone());
                    Ok(())
                })?;
            }
        }
    }
    if !errors.is_empty() {
        return Err(AppError::new(
            "prune_failed",
            format!(
                "deleted {} of {}; failed: {}",
                deleted.len(),
                candidates.len(),
                errors.join("; ")
            ),
        ));
    }
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "candidates": candidates.iter().map(described).collect::<Vec<_>>(),
            "deleted": deleted,
            "skipped": skipped,
            "aborted": false,
        }));
    } else {
        for value in &deleted {
            ctx.out.text(format_args!(
                "deleted {} {} ({})",
                value["project"].as_str().unwrap_or_default(),
                value["name"].as_str().unwrap_or_default(),
                value["id"].as_str().unwrap_or_default()
            ));
        }
        for value in &skipped {
            ctx.out.text(format_args!(
                "skipped {} {}: it is in use again",
                value["project"].as_str().unwrap_or_default(),
                value["name"].as_str().unwrap_or_default()
            ));
        }
    }
    Ok(0)
}

/// Ask on stderr and read one answer from stdin. Only "y" or "yes" agree;
/// end of input declines.
fn confirm(question: &str) -> Result<bool, AppError> {
    eprint!("boatparty: {question}");
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| AppError::io("cannot read the answer", error))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        let now = 10_000_000;
        assert_eq!(ago(0, now), "never");
        assert_eq!(ago(now - 5_000, now), "5s ago");
        assert_eq!(ago(now - 180_000, now), "3m ago");
        assert_eq!(ago(now - 7_200_000, now), "2h ago");
        assert_eq!(ago(now, now + 2 * 86_400_000), "2d ago");
    }
}
