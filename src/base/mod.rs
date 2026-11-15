//! A project's base: a saved snapshot of a warm checkout that new pool
//! members start from.
//!
//! `base build` (`commands::base::build`) brings the project's builder member up (role `builder`,
//! never leased to a workspace, not counted against `max_sandboxes`, kept
//! stopped between builds), syncs the base ref, runs setup, ensure and the
//! warm command, settles the checkout, waits for hydration, stops the
//! builder and saves it as `bp-<project>`. Only then is the record in
//! `state.json` replaced, so a failed build leaves the previous base (record
//! and saved snapshot) usable.
//!
//! A pool member created while a base exists starts `from` it and inherits
//! the base's sync knowledge (`sync.known`, `sync.refs_digest`), so its
//! first sync sends only what changed since the base commit and skips
//! GitHub seeding (which runs only for an empty `known`). If that knowledge
//! is ever wrong, the runner's completeness check answers `needfull` and a
//! full pack follows: never a wrong checkout.

pub mod reference;
pub mod staleness;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::state::{ProjectState, SyncState};

/// The saved snapshot that is project `project`'s base.
pub fn snapshot_name(project: &str) -> String {
    format!("bp-{project}")
}

/// The builder member's name.
pub fn builder_name(project: &str) -> String {
    format!("bp-{project}-builder")
}

/// `projects.<project>.base` in `state.json`. The first six fields are the
/// fixed interface; the rest were added later and may be absent in older
/// records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseRecord {
    /// The saved snapshot's name.
    pub snapshot: String,
    /// The commit the base's checkout holds.
    pub commit: String,
    /// Objects confirmed present in the base's repository.
    #[serde(default)]
    pub known: Vec<String>,
    /// The digest of the refs mirrored into the base.
    #[serde(default)]
    pub refs_digest: Option<String>,
    /// When the saved snapshot became ready (ms since the epoch).
    pub built_at: u64,
    /// The builder sandbox it was saved from.
    pub builder_id: String,
    /// The history mode the base was synced in (`head` or `full`).
    #[serde(default = "head_mode")]
    pub mode: String,
    /// The ref the commit came from (`refs/remotes/origin/main`, `HEAD`).
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    /// The branch checked out in the base, if any.
    #[serde(default)]
    pub branch: Option<String>,
    /// The saved snapshot's restored size, when the provider said.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn head_mode() -> String {
    "head".to_string()
}

impl BaseRecord {
    /// The sync knowledge a member created from this base starts with.
    pub fn seeded_sync(&self) -> SyncState {
        SyncState {
            mode: self.mode.clone(),
            known: self.known.clone(),
            refs_digest: self.refs_digest.clone(),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The project's base record, if one is recorded and readable. A record
/// this binary cannot read counts as no base (and is left as it is).
pub fn record(project: &ProjectState) -> Option<BaseRecord> {
    project
        .base
        .as_ref()
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_with_the_fixed_fields_and_unknown_ones() {
        let value = serde_json::json!({
            "snapshot": "bp-app", "commit": "c1", "known": ["c1", "r1"],
            "refs_digest": "d", "built_at": 5, "builder_id": "bx_b",
            "mode": "full", "ref": "refs/remotes/origin/main", "branch": "main",
            "size_bytes": 10, "later": true
        });
        let project = ProjectState {
            base: Some(value.clone()),
            ..ProjectState::default()
        };
        let base = record(&project).unwrap();
        assert_eq!(base.snapshot, "bp-app");
        assert_eq!(base.git_ref.as_deref(), Some("refs/remotes/origin/main"));
        assert_eq!(base.to_value(), value);
        let sync = base.seeded_sync();
        assert_eq!(sync.mode, "full");
        assert_eq!(sync.known, ["c1", "r1"]);
        assert_eq!(sync.refs_digest.as_deref(), Some("d"));
        assert_eq!(record(&ProjectState::default()), None);
    }

    #[test]
    fn names_follow_the_project() {
        assert_eq!(snapshot_name("app"), "bp-app");
        assert_eq!(builder_name("app"), "bp-app-builder");
    }
}
