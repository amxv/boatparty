//! Vendor-neutral saved-snapshot types: the frozen disk a project's base
//! is, and what the account's plan includes of them.

/// Where a save stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStatus {
    /// The capture is still running.
    Saving,
    /// New sandboxes can start from it.
    Ready,
    /// The save did not complete. A name that held an earlier save keeps
    /// deploying that one.
    Failed,
}

impl SnapshotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Saving => "saving",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// One saved snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSnapshot {
    pub name: String,
    pub status: SnapshotStatus,
    /// Why the save failed, when it did.
    pub error: Option<String>,
    /// The sandbox it was saved from.
    pub source_id: Option<String>,
    /// The restored size of its disk, in bytes.
    pub size_bytes: Option<u64>,
    /// When it was saved (RFC 3339).
    pub created_at: Option<String>,
    /// The frozen artifact behind the name, once ready. A save under an
    /// existing name gets a new one when it is ready.
    pub artifact: Option<String>,
}

/// What the account's plan includes of saved snapshots and what it keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotAllowance {
    pub included: Option<u64>,
    pub used: Option<u64>,
    /// Saved snapshots above the included ones, each paid daily.
    pub extra: Option<u64>,
    pub extra_monthly_dollars: Option<f64>,
    pub price_per_extra_daily_dollars: Option<f64>,
}
