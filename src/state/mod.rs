//! `state.json`: pools, leases, affinity, bases, the start ledger and the
//! limits cache, shared by every boatparty process of this user.
//!
//! Field names and nesting are a fixed interface; fields may be added.
//! Unknown fields are kept on write, so an older binary never drops what a
//! newer one stored. Times are milliseconds since the Unix epoch.

mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use store::Store;

use crate::system;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectState>,
    #[serde(default)]
    pub starts: Vec<StartRecord>,
    #[serde(default)]
    pub limits_cache: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            projects: BTreeMap::new(),
            starts: Vec::new(),
            limits_cache: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectState {
    #[serde(default)]
    pub members: Vec<Member>,
    /// Last failed claim by member name, including refused pending creates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bring_up_failures: BTreeMap<String, BringUpFailure>,
    #[serde(default)]
    pub base: Option<Value>,
    #[serde(default)]
    pub refresh_pid: Option<u32>,
    #[serde(default)]
    pub last_refresh_error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Pool,
    Builder,
    Gate,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::Builder => "builder",
            Self::Gate => "gate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// The provider's id; `None` while a create is pending.
    pub id: Option<String>,
    /// The idempotency key of the pending or completed create.
    pub create_key: String,
    pub name: String,
    pub role: Role,
    /// The workspace this member last served.
    pub affinity: Option<String>,
    #[serde(default)]
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub bringing_up: Option<BringingUp>,
    #[serde(default)]
    pub sync: SyncState,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_used_at: u64,
    #[serde(default)]
    pub last_run: Option<Value>,
    /// Why the next bring-up must attempt recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broken: Option<Broken>,
    #[serde(default)]
    pub recovery: Recovery,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Durable facts about this physical sandbox, reset on replacement.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recovery {
    pub restoring: bool,
    pub force_stopping: bool,
    pub restarted_at: Option<u64>,
    /// Unix seconds of the snapshot restored by the last forced stop.
    pub snapshot_seconds: Option<u64>,
    pub rebooted_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BringUpFailure {
    pub pid: u32,
    pub started_at: u64,
    pub message: String,
}

/// Why and when a member was found broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Broken {
    pub reason: String,
    /// Milliseconds since the epoch.
    pub at: u64,
}

impl Member {
    /// The leases whose process is still alive.
    pub fn live_leases(&self) -> impl Iterator<Item = &Lease> {
        self.leases
            .iter()
            .filter(|lease| system::pid_alive(lease.pid))
    }

    pub fn has_live_lease(&self) -> bool {
        self.live_leases().next().is_some()
    }

    /// Another live process is bringing this member up.
    pub fn bring_up_claimed_by_other(&self) -> bool {
        self.bringing_up
            .as_ref()
            .is_some_and(|claim| claim.pid != std::process::id() && system::pid_alive(claim.pid))
    }

    /// In use: a live lease, or a live process bringing it up.
    pub fn busy(&self) -> bool {
        self.has_live_lease() || self.bring_up_claimed_by_other()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseKind {
    Run,
    Serve,
    Session,
}

impl LeaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Serve => "serve",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub pid: u32,
    pub kind: LeaseKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub started_at: u64,
    /// The workspace the holder works for (older records have none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BringingUp {
    pub pid: u32,
    pub started_at: u64,
    #[serde(default)]
    pub step: String,
    #[serde(default)]
    pub since: u64,
    #[serde(default = "default_step_seconds")]
    pub limit_seconds: u64,
}

fn default_step_seconds() -> u64 {
    600
}

impl BringingUp {
    pub fn new(pid: u32, now: u64) -> Self {
        Self {
            pid,
            started_at: now,
            step: "checking sandbox state".into(),
            since: now,
            limit_seconds: 120,
        }
    }

    pub fn wedged(&self, now: u64) -> bool {
        now.saturating_sub(self.since.max(self.started_at)) / 1000
            > self.limit_seconds.saturating_add(60)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    pub mode: String,
    #[serde(default)]
    pub known: Vec<String>,
    #[serde(default)]
    pub refs_digest: Option<String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            mode: "head".to_string(),
            known: Vec::new(),
            refs_digest: None,
        }
    }
}

/// One start in the `StartLedger`: a reservation made before a create or
/// resume, kept when the provider counted the start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecord {
    /// When the start was reserved.
    pub at: u64,
    pub project: String,
    /// Identifies the reservation (older records have none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The process that reserved it, while the start is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When the provider's answer arrived; `None` while the call is in
    /// flight. A start done before a limits fetch began is in that figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<u64>,
}

impl State {
    pub fn project(&self, name: &str) -> Option<&ProjectState> {
        self.projects.get(name)
    }

    pub fn project_mut(&mut self, name: &str) -> &mut ProjectState {
        self.projects.entry(name.to_string()).or_default()
    }
}

impl ProjectState {
    /// The pool member whose affinity is this workspace.
    pub fn affinity_member(&self, workspace: &str) -> Option<&Member> {
        self.members.iter().find(|member| {
            member.role == Role::Pool && member.affinity.as_deref() == Some(workspace)
        })
    }

    /// A member by its name or its provider id.
    pub fn find(&self, key: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.name == key || member.id.as_deref() == Some(key))
    }

    pub fn find_mut_by_key(&mut self, create_key: &str) -> Option<&mut Member> {
        self.members
            .iter_mut()
            .find(|member| member.create_key == create_key)
    }

    pub fn remove_by_key(&mut self, create_key: &str) -> Option<Member> {
        let index = self
            .members
            .iter()
            .position(|member| member.create_key == create_key)?;
        Some(self.members.remove(index))
    }

    /// The next free pool name, `bp-<project>-<n>` with the smallest unused `n`.
    pub fn next_member_name(&self, project: &str) -> String {
        let prefix = format!("bp-{project}-");
        let used: Vec<u32> = self
            .members
            .iter()
            .filter_map(|member| member.name.strip_prefix(&prefix)?.parse().ok())
            .collect();
        let n = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        format!("{prefix}{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_shape_round_trips_and_keeps_unknown_fields() {
        let text = r#"{
          "version": 1,
          "projects": {
            "app": {
              "members": [{
                "id": null, "create_key": "k1", "name": "bp-app-3", "role": "pool",
                "affinity": "/w", "leases": [{"pid": 1, "kind": "run", "label": "cargo test", "started_at": 0}],
                "bringing_up": null,
                "sync": {"mode": "head", "known": ["abc"], "refs_digest": null},
                "created_at": 0, "last_used_at": 0, "last_run": null, "future": 7
              }],
              "base": null
            }
          },
          "starts": [{"at": 0, "project": "app"}],
          "limits_cache": null,
          "added_later": {"x": 1}
        }"#;
        let state: State = serde_json::from_str(text).unwrap();
        let member = &state.projects["app"].members[0];
        assert_eq!(member.id, None);
        assert_eq!(member.leases[0].kind, LeaseKind::Run);
        let written = serde_json::to_value(&state).unwrap();
        assert_eq!(written["added_later"]["x"], 1);
        assert_eq!(written["projects"]["app"]["members"][0]["future"], 7);
        assert_eq!(written["projects"]["app"]["members"][0]["id"], Value::Null);
        assert_eq!(written["projects"]["app"]["base"], Value::Null);
        assert_eq!(written["limits_cache"], Value::Null);
    }

    #[test]
    fn member_names_fill_the_smallest_gap() {
        let mut project = ProjectState::default();
        assert_eq!(project.next_member_name("app"), "bp-app-1");
        for name in ["bp-app-1", "bp-app-3"] {
            project.members.push(Member {
                id: None,
                create_key: name.into(),
                name: name.into(),
                role: Role::Pool,
                affinity: None,
                leases: Vec::new(),
                bringing_up: None,
                sync: SyncState::default(),
                created_at: 0,
                last_used_at: 0,
                last_run: None,
                broken: None,
                recovery: Default::default(),
                extra: Map::new(),
            });
        }
        assert_eq!(project.next_member_name("app"), "bp-app-2");
    }

    #[test]
    fn dead_lease_holders_do_not_count() {
        let mut member: Member = serde_json::from_value(serde_json::json!({
            "id": "bx_1", "create_key": "k", "name": "bp-app-1", "role": "pool", "affinity": null
        }))
        .unwrap();
        assert!(!member.has_live_lease());
        member.leases.push(Lease {
            pid: u32::MAX,
            kind: LeaseKind::Session,
            label: String::new(),
            started_at: 0,
            workspace: None,
        });
        assert!(!member.has_live_lease());
        member.leases.push(Lease {
            pid: std::process::id(),
            kind: LeaseKind::Run,
            label: String::new(),
            started_at: 0,
            workspace: None,
        });
        assert!(member.has_live_lease());
    }
}
