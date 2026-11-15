//! A project's pool of sandboxes: which member serves a workspace, and
//! bringing that member up.
//!
//! Acquisition follows [`policy`]: the workspace's affinity member, a free
//! member, a new member below the cap, an idle member of another workspace,
//! or a bounded wait. Every provider call and every `ssh` process runs
//! outside the state lock; the lock covers only the record changes around
//! them.

pub mod acquire;
mod bringup;
pub mod lease;
pub mod policy;
pub mod reconcile;
pub mod remove;
pub mod wait;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

pub use acquire::LeaseRequest;
pub use bringup::{BringUp, Up, ensure_connected};

use crate::account::config::AccountConfig;
use crate::error::AppError;
use crate::output::Output;
use crate::paths::Paths;
use crate::project::Workspace;
use crate::provider::{Provider, SandboxState};
use crate::state::{Member, Role, State, Store, SyncState};
use crate::system;
use crate::transport::ssh::SshTarget;
use acquire::{Facts, How, Outcome, Request};
use policy::{Limits, Reassign};

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

/// A project's pool limits, from `account.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolLimits {
    pub max_sandboxes: u32,
    pub steal_after_minutes: u32,
    pub acquire_wait: Duration,
}

impl PoolLimits {
    /// `[defaults]`, with the project's own `max_sandboxes` when it sets one.
    pub fn for_project(config: &AccountConfig, project: &str) -> Self {
        let defaults = &config.defaults;
        Self {
            max_sandboxes: config
                .projects
                .get(project)
                .and_then(|entry| entry.max_sandboxes)
                .unwrap_or(defaults.max_sandboxes),
            steal_after_minutes: defaults.steal_after_minutes,
            acquire_wait: Duration::from_secs(u64::from(defaults.acquire_wait_seconds)),
        }
    }

    pub fn policy(&self) -> Limits {
        Limits {
            max_sandboxes: self.max_sandboxes,
            steal_after_ms: u64::from(self.steal_after_minutes) * 60_000,
        }
    }
}

/// Whether a workspace's directory exists. A missing one only ever frees
/// its member; nothing is deleted because of it.
pub fn workspace_exists(workspace: &str) -> bool {
    Path::new(workspace).is_dir()
}

/// What a command needs to work on one workspace's pool.
pub struct Pool<'a> {
    pub paths: &'a Paths,
    pub store: Store,
    pub provider: &'a dyn Provider,
    pub out: Output,
    pub project: String,
    pub workspace: Workspace,
    pub limits: PoolLimits,
}

impl<'a> Pool<'a> {
    pub fn new(
        paths: &'a Paths,
        provider: &'a dyn Provider,
        out: Output,
        project: String,
        workspace: Workspace,
        limits: PoolLimits,
    ) -> Self {
        Self {
            paths,
            store: Store::new(paths),
            provider,
            out,
            project,
            workspace,
            limits,
        }
    }

    /// A member for this workspace, by the acquisition order, with `lease`
    /// taken in the same write. A new member's pending record exists before
    /// any provider call is made for it. Waits up to `acquire_wait` when
    /// every member is in use at the cap. Returns the member and whether it
    /// is a record this call just created.
    pub fn acquire(&self, lease: Option<&LeaseRequest>) -> Result<(Member, bool), AppError> {
        let workspace = self.workspace.key();
        let deadline = Instant::now() + self.limits.acquire_wait;
        let mut running: Option<BTreeMap<String, bool>> = None;
        let mut announced = false;
        loop {
            if running.is_none() && self.needs_running(&workspace)? {
                running = Some(self.running_members());
            }
            let empty = BTreeMap::new();
            let facts = Facts {
                running: running.as_ref().unwrap_or(&empty),
                exists: &workspace_exists,
            };
            let key = system::uuid_v4()?;
            let request = Request {
                project: &self.project,
                workspace: &workspace,
                limits: self.limits.policy(),
                lease,
                pid: std::process::id(),
                new_key: &key,
                now: system::now_ms(),
            };
            let outcome = self
                .store
                .update(|state| Ok(acquire::acquire(state, &request, &facts)))?;
            match outcome {
                Outcome::Acquired { member, how } => {
                    self.announce(&member, &how);
                    return Ok((*member, how == How::Created));
                }
                Outcome::Wait { members } => {
                    if Instant::now() >= deadline {
                        return Err(self.exhausted(members));
                    }
                    if !announced {
                        announced = true;
                        self.out.note(format_args!(
                            "all {members} sandboxes of project {} are in use (max_sandboxes = {}); \
                             waiting up to {}s for one to become free",
                            self.project,
                            self.limits.max_sandboxes,
                            self.limits.acquire_wait.as_secs()
                        ));
                    }
                    std::thread::sleep(
                        wait::poll_interval()
                            .saturating_mul(2)
                            .min(deadline.saturating_duration_since(Instant::now()))
                            .max(Duration::from_millis(1)),
                    );
                }
            }
        }
    }

    /// Whether the choice may depend on which members run: this workspace
    /// has no member yet and the pool has members with a sandbox.
    fn needs_running(&self, workspace: &str) -> Result<bool, AppError> {
        let state = self.store.load()?;
        Ok(state.project(&self.project).is_some_and(|pool| {
            pool.affinity_member(workspace).is_none()
                && pool
                    .members
                    .iter()
                    .any(|member| member.role == Role::Pool && member.id.is_some())
        }))
    }

    /// Which members run, from one provider listing. Empty when the listing
    /// fails: running is only a preference.
    fn running_members(&self) -> BTreeMap<String, bool> {
        self.provider
            .list_by_label(LABEL_PROJECT, &self.project)
            .map(|listed| {
                listed
                    .into_iter()
                    .map(|sandbox| (sandbox.id, sandbox.state == SandboxState::Running))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn announce(&self, member: &Member, how: &How) {
        match how {
            How::Affinity | How::Created => {}
            How::Free => self
                .out
                .note(format_args!("using the free sandbox {}", member.name)),
            How::Reassigned { from, why } => self.out.note(format_args!(
                "taking over {} from {from} ({})",
                member.name,
                match why {
                    Reassign::WorkspaceGone => "that workspace no longer exists".to_string(),
                    Reassign::Idle => format!(
                        "unused for more than {} minutes",
                        self.limits.steal_after_minutes
                    ),
                }
            )),
        }
    }

    fn exhausted(&self, members: usize) -> AppError {
        AppError::new(
            "pool_exhausted",
            format!(
                "no sandbox of project {} became free within {}s: all {members} are in use and \
                 max_sandboxes is {}",
                self.project,
                self.limits.acquire_wait.as_secs(),
                self.limits.max_sandboxes
            ),
        )
        .with_hint(format!(
            "raise the limit with `max_sandboxes = N` under [projects.{}] (or [defaults]) in \
             account.toml, or run `boatparty release` in a workspace that no longer needs its \
             sandbox; `boatparty pool` shows who holds them",
            self.project
        ))
    }

    /// A member of this project by name or id, or this workspace's member.
    pub fn resolve(&self, key: Option<&str>) -> Result<Member, AppError> {
        let state = self.store.load()?;
        resolve_in(&state, &self.project, &self.workspace.key(), key)
    }

    /// Refuse when a live process holds a lease on the member or is
    /// bringing it up.
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
        let mut live: Vec<String> = current
            .live_leases()
            .map(|lease| format!("pid {} ({})", lease.pid, lease.label))
            .collect();
        if let Some(claim) = &current.bringing_up
            && current.bring_up_claimed_by_other()
        {
            live.push(format!("pid {} (starting it)", claim.pid));
        }
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

    /// Acquire this workspace's member (taking `lease` with it) and bring it
    /// up. One process brings a member up; others wait and then reuse its
    /// connection.
    pub fn bring_up(&self, options: BringUp, lease: Option<&LeaseRequest>) -> Result<Up, AppError> {
        bringup::bring_up(self, options, lease)
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
