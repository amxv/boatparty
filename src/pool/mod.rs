//! A project's pool of sandboxes: which member serves a workspace, and
//! bringing that member up.
//!
//! Acquisition in this phase: the workspace's affinity member, else a new
//! member. Every provider call and every `ssh` process runs outside the
//! state lock; the lock covers only the record changes around them.

mod bringup;
pub mod lease;
pub mod wait;

use std::collections::BTreeMap;

pub use bringup::{BringUp, Up, ensure_connected};

use crate::error::AppError;
use crate::output::Output;
use crate::paths::Paths;
use crate::project::Workspace;
use crate::provider::Provider;
use crate::state::{Member, Role, State, Store, SyncState};
use crate::system;
use crate::transport::ssh::SshTarget;

pub const LABEL_PROJECT: &str = "boatparty.project";
pub const LABEL_ROLE: &str = "boatparty.role";

/// The labels every pool member carries at the provider.
pub fn member_labels(project: &str, role: Role) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_PROJECT.to_string(), project.to_string()),
        (LABEL_ROLE.to_string(), role.as_str().to_string()),
    ])
}

/// The checkout directory on every member of a project.
pub fn remote_checkout(project: &str) -> String {
    format!("/home/user/{project}")
}

/// This workspace's member and its SSH target, when the member's control
/// master is alive. Reads the state without the lock and makes no provider
/// call, so warm commands cost one `ssh -O check`.
pub fn warm_member(
    paths: &Paths,
    project: &str,
    workspace: &str,
) -> Result<Option<(Member, SshTarget)>, AppError> {
    let state = Store::new(paths).load()?;
    let Some(member) = state
        .project(project)
        .and_then(|pool| pool.affinity_member(workspace))
    else {
        return Ok(None);
    };
    let Some(id) = member.id.as_deref() else {
        return Ok(None);
    };
    let target = SshTarget::for_sandbox(paths, id);
    Ok(target.master_alive().then(|| (member.clone(), target)))
}

/// What a command needs to work on one workspace's pool.
pub struct Pool<'a> {
    pub paths: &'a Paths,
    pub store: Store,
    pub provider: &'a dyn Provider,
    pub out: Output,
    pub project: String,
    pub workspace: Workspace,
}

impl<'a> Pool<'a> {
    pub fn new(
        paths: &'a Paths,
        provider: &'a dyn Provider,
        out: Output,
        project: String,
        workspace: Workspace,
    ) -> Self {
        Self {
            paths,
            store: Store::new(paths),
            provider,
            out,
            project,
            workspace,
        }
    }

    /// This workspace's member, creating a pending record when there is none.
    /// The record exists before any provider call is made for it.
    pub fn acquire(&self) -> Result<(Member, bool), AppError> {
        let key = system::uuid_v4()?;
        let workspace = self.workspace.key();
        let project = self.project.clone();
        self.store.update(|state| {
            let pool = state.project_mut(&project);
            if let Some(member) = pool.affinity_member(&workspace) {
                return Ok((member.clone(), false));
            }
            let now = system::now_ms();
            let member = new_member(pool.next_member_name(&project), key, Some(workspace), now);
            pool.members.push(member.clone());
            Ok((member, true))
        })
    }

    /// A member of this project by name or id, or this workspace's member.
    pub fn resolve(&self, key: Option<&str>) -> Result<Member, AppError> {
        let state = self.store.load()?;
        resolve_in(&state, &self.project, &self.workspace.key(), key)
    }

    /// Refuse when a live process holds a lease on the member.
    pub fn refuse_if_leased(&self, member: &Member, action: &str) -> Result<(), AppError> {
        let state = self.store.load()?;
        let current = state
            .project(&self.project)
            .and_then(|pool| {
                pool.members
                    .iter()
                    .find(|other| other.create_key == member.create_key)
            })
            .unwrap_or(member);
        let live: Vec<String> = current
            .live_leases()
            .map(|lease| format!("pid {} ({})", lease.pid, lease.label))
            .collect();
        if live.is_empty() {
            return Ok(());
        }
        Err(AppError::new(
            "member_in_use",
            format!(
                "cannot {action} {}: it is in use by {}",
                current.name,
                live.join(", ")
            ),
        )
        .with_hint("wait for those commands to finish"))
    }

    /// Change a record (matched by its create key) under the lock and return
    /// the result. A record that disappeared meanwhile is put back.
    pub fn modify(
        &self,
        member: &Member,
        change: impl FnOnce(&mut Member),
    ) -> Result<Member, AppError> {
        let project = self.project.clone();
        self.store.update(|state| {
            let pool = state.project_mut(&project);
            let slot = match pool.find_mut_by_key(&member.create_key) {
                Some(slot) => slot,
                None => {
                    pool.members.push(member.clone());
                    pool.members.last_mut().expect("just pushed")
                }
            };
            change(slot);
            Ok(slot.clone())
        })
    }

    /// Drop a record (matched by its create key).
    pub fn forget(&self, member: &Member) -> Result<(), AppError> {
        let project = self.project.clone();
        self.store.update(|state| {
            state
                .project_mut(&project)
                .remove_by_key(&member.create_key);
            Ok(())
        })
    }

    pub fn bring_up(&self, options: BringUp) -> Result<Up, AppError> {
        bringup::bring_up(self, options)
    }
}

pub fn new_member(name: String, create_key: String, affinity: Option<String>, now: u64) -> Member {
    Member {
        id: None,
        create_key,
        name,
        role: Role::Pool,
        affinity,
        leases: Vec::new(),
        bringing_up: None,
        sync: SyncState::default(),
        created_at: now,
        last_used_at: now,
        last_run: None,
        extra: serde_json::Map::new(),
    }
}

fn resolve_in(
    state: &State,
    project: &str,
    workspace: &str,
    key: Option<&str>,
) -> Result<Member, AppError> {
    let pool = state.project(project);
    let found = match key {
        Some(key) => pool.and_then(|pool| pool.find(key)),
        None => pool.and_then(|pool| pool.affinity_member(workspace)),
    };
    found.cloned().ok_or_else(|| match key {
        Some(key) => AppError::new(
            "no_member",
            format!("project {project} has no sandbox named {key}"),
        )
        .with_hint("`boatparty status` lists them"),
        None => AppError::new(
            "no_member",
            format!("this workspace ({workspace}) has no sandbox in project {project}"),
        )
        .with_hint("start one with `boatparty launch`"),
    })
}
