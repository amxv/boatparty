//! Reconciling a project's records with the sandboxes the provider labels
//! as the project's, and pruning stopped members nobody needs.
//!
//! - A labelled pool sandbox with no record becomes a free member (or fills
//!   in the id of a pending record with its name).
//! - A record whose sandbox is gone is dropped.
//! - A member whose stored disk the provider deleted (data expired) is
//!   deleted and dropped.
//! - A member whose workspace directory no longer exists loses its affinity.
//!
//! Nothing is ever deleted because a local path is missing. Members in use
//! (a live lease or a live bring-up) are left alone.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Serialize;

use super::remove::delete_sandbox;
use super::{LABEL_PROJECT, LABEL_ROLE, new_member, wait, workspace_exists};
use crate::error::AppError;
use crate::output::Output;
use crate::paths::Paths;
use crate::provider::{Provider, Sandbox, SandboxState};
use crate::state::{Member, ProjectState, Role, Store};
use crate::system;
use crate::transport::ssh::SshTarget;

/// A member named in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Named {
    pub name: String,
    pub id: Option<String>,
}

impl Named {
    fn of(member: &Member) -> Self {
        Self {
            name: member.name.clone(),
            id: member.id.clone(),
        }
    }
}

/// What reconciling one project changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub adopted: Vec<Named>,
    pub dropped: Vec<Named>,
    pub expired: Vec<Named>,
    /// Members that lost the affinity of a workspace that no longer exists:
    /// (member, workspace).
    pub freed: Vec<(Named, String)>,
    /// Data-expired members whose deletion failed and whose record was kept.
    pub errors: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.adopted.is_empty()
            && self.dropped.is_empty()
            && self.expired.is_empty()
            && self.freed.is_empty()
            && self.errors.is_empty()
    }
}

fn is_pool_sandbox(sandbox: &Sandbox) -> bool {
    sandbox.state != SandboxState::Gone
        && sandbox
            .labels
            .get(LABEL_ROLE)
            .is_none_or(|role| role == Role::Pool.as_str())
}

/// Apply what the provider reported to one project's records. `listed` is
/// the label listing; `gone` the ids of records the provider no longer
/// knows. Returns the changes and the removed data-expired members, which
/// the caller deletes at the provider. Pure apart from pid liveness.
pub fn apply(
    pool: &mut ProjectState,
    listed: &[Sandbox],
    gone: &BTreeSet<String>,
    exists: &dyn Fn(&str) -> bool,
    new_key: &mut dyn FnMut() -> String,
    now: u64,
) -> (Changes, Vec<Member>) {
    let mut changes = Changes::default();
    let known: BTreeSet<String> = pool
        .members
        .iter()
        .filter_map(|member| member.id.clone())
        .collect();
    for sandbox in listed
        .iter()
        .filter(|sandbox| is_pool_sandbox(sandbox) && !known.contains(&sandbox.id))
    {
        let name = sandbox.name.clone();
        // A pending record of the same name: its create succeeded but the
        // answer was lost. One being brought up is left to that process.
        if let Some(pending) = pool
            .members
            .iter_mut()
            .find(|member| member.id.is_none() && name.as_deref() == Some(member.name.as_str()))
        {
            if !pending.bring_up_claimed_by_other() {
                pending.id = Some(sandbox.id.clone());
                changes.adopted.push(Named::of(pending));
            }
            continue;
        }
        let name = match name {
            Some(name) if pool.find(&name).is_none() => name,
            _ => sandbox.id.clone(),
        };
        let mut member = new_member(name, new_key(), None, now);
        member.id = Some(sandbox.id.clone());
        member.last_used_at = 0;
        changes.adopted.push(Named::of(&member));
        pool.members.push(member);
    }

    let expired_ids: BTreeSet<&str> = listed
        .iter()
        .filter(|sandbox| sandbox.data_expired_at.is_some() && sandbox.state != SandboxState::Gone)
        .map(|sandbox| sandbox.id.as_str())
        .collect();
    let mut expired = Vec::new();
    let mut kept = Vec::with_capacity(pool.members.len());
    for mut member in std::mem::take(&mut pool.members) {
        let id = member.id.clone().unwrap_or_default();
        let in_use = member.busy();
        if member.id.is_some() && gone.contains(&id) && !member.bring_up_claimed_by_other() {
            changes.dropped.push(Named::of(&member));
            continue;
        }
        if member.id.is_some() && expired_ids.contains(id.as_str()) && !in_use {
            changes.expired.push(Named::of(&member));
            expired.push(member);
            continue;
        }
        if let Some(workspace) = member.affinity.clone()
            && member.role == Role::Pool
            && !in_use
            && !exists(&workspace)
        {
            member.affinity = None;
            member.leases.clear();
            changes.freed.push((Named::of(&member), workspace));
        }
        kept.push(member);
    }
    pool.members = kept;
    (changes, expired)
}

fn retry<T>(op: impl FnMut() -> Result<T, crate::provider::ProviderError>) -> Result<T, AppError> {
    Ok(wait::retry(Duration::from_secs(60), wait::transient, op)?)
}

/// The project's labelled sandboxes, by id.
pub fn list(provider: &dyn Provider, project: &str) -> Result<Vec<Sandbox>, AppError> {
    retry(|| provider.list_by_label(LABEL_PROJECT, project))
}

/// Reconcile one project against the provider. Returns the changes and the
/// listing it was based on.
pub fn reconcile(
    paths: &Paths,
    provider: &dyn Provider,
    out: Output,
    project: &str,
) -> Result<(Changes, Vec<Sandbox>), AppError> {
    let store = Store::new(paths);
    let listed = list(provider, project)?;
    let listed_ids: BTreeSet<&str> = listed.iter().map(|sandbox| sandbox.id.as_str()).collect();
    let mut gone = BTreeSet::new();
    let records = store
        .load()?
        .project(project)
        .map(|pool| pool.members.clone())
        .unwrap_or_default();
    for id in records.iter().filter_map(|member| member.id.as_deref()) {
        if !listed_ids.contains(id) && retry(|| provider.get(id))?.state == SandboxState::Gone {
            gone.insert(id.to_string());
        }
    }
    for sandbox in &listed {
        if sandbox.state == SandboxState::Gone {
            gone.insert(sandbox.id.clone());
        }
    }
    let mut keys = Vec::new();
    for _ in 0..listed.len() {
        keys.push(system::uuid_v4()?);
    }
    let (mut changes, expired) = store.update(|state| {
        let mut keys = keys.into_iter();
        let mut new_key = || keys.next().unwrap_or_default();
        Ok(apply(
            state.project_mut(project),
            &listed,
            &gone,
            &workspace_exists,
            &mut new_key,
            system::now_ms(),
        ))
    })?;
    for named in &changes.dropped {
        if let Some(id) = &named.id {
            SshTarget::for_sandbox(paths, id).remove_files();
        }
    }
    for member in expired {
        let id = member.id.clone().unwrap_or_default();
        out.note(format_args!(
            "{} ({id}) was stopped so long that the provider deleted its disk; deleting it",
            member.name
        ));
        if let Err(error) = delete_sandbox(paths, provider, out, &member.name, &id) {
            changes.errors.push(format!("{}: {error}", member.name));
            changes
                .expired
                .retain(|named| named.id.as_deref() != Some(&id));
            let _ = store.update(|state| {
                state.project_mut(project).members.push(member.clone());
                Ok(())
            });
        }
    }
    Ok((changes, listed))
}

/// A stopped member nobody needs: no affinity, or the affinity of a
/// workspace that no longer exists, and not in use.
pub fn prunable(
    member: &Member,
    state: Option<SandboxState>,
    exists: &dyn Fn(&str) -> bool,
) -> bool {
    member.role == Role::Pool
        && member.id.is_some()
        && state == Some(SandboxState::Stopped)
        && !member.busy()
        && member
            .affinity
            .as_deref()
            .is_none_or(|workspace| !exists(workspace))
}

/// Provider states by id.
pub fn states(listed: &[Sandbox]) -> BTreeMap<String, SandboxState> {
    listed
        .iter()
        .map(|sandbox| (sandbox.id.clone(), sandbox.state))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::provider::Hydration;
    use crate::state::{Lease, LeaseKind};

    fn sandbox(id: &str, name: &str, state: SandboxState) -> Sandbox {
        Sandbox {
            id: id.into(),
            name: Some(name.into()),
            state,
            hydration: Hydration::Complete,
            labels: BTreeMap::from([
                (LABEL_PROJECT.to_string(), "app".to_string()),
                (LABEL_ROLE.to_string(), "pool".to_string()),
            ]),
            size: None,
            auto_stop_at: None,
            data_expired_at: None,
            error: None,
        }
    }

    fn member(name: &str, id: Option<&str>, affinity: Option<&str>) -> Member {
        let mut member = new_member(
            name.into(),
            format!("key-{name}"),
            affinity.map(Into::into),
            0,
        );
        member.id = id.map(Into::into);
        member
    }

    fn run(pool: &mut ProjectState, listed: &[Sandbox], gone: &[&str]) -> (Changes, Vec<Member>) {
        let gone: BTreeSet<String> = gone.iter().map(|id| id.to_string()).collect();
        let mut n = 0;
        let mut new_key = || {
            n += 1;
            format!("new-{n}")
        };
        apply(
            pool,
            listed,
            &gone,
            &|path: &str| path != "/gone",
            &mut new_key,
            7,
        )
    }

    #[test]
    fn an_untracked_labelled_sandbox_becomes_a_free_member() {
        let mut pool = ProjectState::default();
        pool.members
            .push(member("bp-app-1", Some("bx_1"), Some("/w")));
        let mut builder = sandbox("bx_b", "bp-app-builder", SandboxState::Stopped);
        builder
            .labels
            .insert(LABEL_ROLE.to_string(), "builder".to_string());
        let listed = [
            sandbox("bx_1", "bp-app-1", SandboxState::Running),
            sandbox("bx_2", "bp-app-2", SandboxState::Stopped),
            // Same name as a tracked member: named by its id instead.
            sandbox("bx_3", "bp-app-1", SandboxState::Running),
            builder,
            sandbox("bx_4", "bp-app-4", SandboxState::Gone),
        ];
        let (changes, expired) = run(&mut pool, &listed, &[]);
        assert!(expired.is_empty());
        let names: Vec<&str> = changes.adopted.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["bp-app-2", "bx_3"]);
        let adopted = pool.find("bx_2").unwrap();
        assert_eq!(adopted.affinity, None);
        assert_eq!(adopted.role, Role::Pool);
        assert_eq!(adopted.create_key, "new-1");
        assert!(pool.find("bx_b").is_none() && pool.find("bx_4").is_none());
        // Idempotent.
        let (changes, _) = run(&mut pool, &listed, &[]);
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn a_pending_record_gets_the_id_of_its_sandbox() {
        let mut pool = ProjectState::default();
        pool.members.push(member("bp-app-1", None, Some("/w")));
        let (changes, _) = run(
            &mut pool,
            &[sandbox("bx_1", "bp-app-1", SandboxState::Running)],
            &[],
        );
        assert_eq!(changes.adopted.len(), 1);
        assert_eq!(pool.members.len(), 1);
        assert_eq!(pool.members[0].id.as_deref(), Some("bx_1"));
        assert_eq!(pool.members[0].affinity.as_deref(), Some("/w"));
    }

    #[test]
    fn gone_records_are_dropped_and_expired_ones_handed_back_for_deletion() {
        let mut pool = ProjectState::default();
        pool.members
            .push(member("bp-app-1", Some("bx_1"), Some("/w")));
        pool.members.push(member("bp-app-2", Some("bx_2"), None));
        pool.members
            .push(member("bp-app-3", Some("bx_3"), Some("/w3")));
        let mut expired = sandbox("bx_2", "bp-app-2", SandboxState::Stopped);
        expired.data_expired_at = Some("2026-11-01T00:00:00Z".into());
        let listed = [expired, sandbox("bx_3", "bp-app-3", SandboxState::Stopped)];
        let (changes, removed) = run(&mut pool, &listed, &["bx_1"]);
        assert_eq!(
            changes.dropped,
            [Named::of(&member("bp-app-1", Some("bx_1"), None))]
        );
        assert_eq!(changes.expired.len(), 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].name, "bp-app-2");
        let names: Vec<&str> = pool.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["bp-app-3"]);
    }

    #[test]
    fn a_missing_workspace_only_frees_its_member_and_nothing_in_use_is_touched() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut leased = member("bp-app-2", Some("bx_2"), Some("/gone"));
        leased.leases.push(Lease {
            pid: child.id(),
            kind: LeaseKind::Run,
            label: String::new(),
            started_at: 0,
            workspace: Some("/gone".into()),
        });
        let mut pool = ProjectState::default();
        pool.members
            .push(member("bp-app-1", Some("bx_1"), Some("/gone")));
        pool.members.push(leased);
        let mut expired = sandbox("bx_2", "bp-app-2", SandboxState::Stopped);
        expired.data_expired_at = Some("2026-11-01T00:00:00Z".into());
        let listed = [sandbox("bx_1", "bp-app-1", SandboxState::Stopped), expired];
        let (changes, removed) = run(&mut pool, &listed, &[]);
        assert_eq!(changes.freed.len(), 1);
        assert_eq!(changes.freed[0].1, "/gone");
        assert!(
            removed.is_empty(),
            "a leased member was handed back for deletion"
        );
        assert_eq!(pool.members.len(), 2, "a missing path deleted a record");
        assert_eq!(pool.members[0].affinity, None);
        assert_eq!(pool.members[1].affinity.as_deref(), Some("/gone"));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn only_stopped_unneeded_members_are_prunable() {
        let exists = |path: &str| path != "/gone";
        let stopped = Some(SandboxState::Stopped);
        assert!(prunable(&member("a", Some("bx_a"), None), stopped, &exists));
        assert!(prunable(
            &member("b", Some("bx_b"), Some("/gone")),
            stopped,
            &exists
        ));
        assert!(!prunable(
            &member("c", Some("bx_c"), Some("/here")),
            stopped,
            &exists
        ));
        assert!(!prunable(
            &member("d", Some("bx_d"), None),
            Some(SandboxState::Running),
            &exists
        ));
        assert!(!prunable(&member("e", None, None), stopped, &exists));
        assert!(!prunable(&member("f", Some("bx_f"), None), None, &exists));
    }
}
