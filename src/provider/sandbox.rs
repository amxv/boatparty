//! Vendor-neutral sandbox lifecycle types.
//!
//! The provider maps its own state names onto [`SandboxState`] and its
//! restore progress onto [`Hydration`]. Nothing outside the provider module
//! sees a vendor state string.

use std::collections::BTreeMap;
use std::fmt;

use super::MachineSize;

/// Where a sandbox is in its lifecycle. Closed: the provider turns an
/// unrecognized state into an error, never into one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxState {
    /// Being provisioned or started; commands are held.
    Starting,
    /// A machine is running; commands and SSH work.
    Running,
    /// Saving its disk before the machine goes away.
    Stopping,
    /// No machine; the disk is kept and a resume brings it back.
    Stopped,
    /// Failed; it needs attention or deletion.
    Error,
    /// It no longer exists at the provider.
    Gone,
}

impl SandboxState {
    /// The plain word used in text and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Gone => "gone",
        }
    }
}

impl fmt::Display for SandboxState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether every file of a restored disk is present. Only `Complete` is safe
/// to stop; `Unknown` (the provider did not say) never counts as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hydration {
    Complete,
    /// Files are still arriving; `percent` when the provider reported it.
    InProgress {
        percent: Option<u8>,
    },
    Unknown,
}

impl Hydration {
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::InProgress { .. } => "in_progress",
            Self::Unknown => "unknown",
        }
    }
}

/// One sandbox as the provider describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    pub id: String,
    pub name: Option<String>,
    pub state: SandboxState,
    pub hydration: Hydration,
    pub labels: BTreeMap<String, String>,
    pub size: Option<MachineSize>,
    /// When the sandbox stops by itself (RFC 3339), or `None` when it never does.
    pub auto_stop_at: Option<String>,
    /// Set when the provider deleted the stored disk; it cannot resume.
    pub data_expired_at: Option<String>,
    /// Why the sandbox failed or stopped, when the provider said.
    pub error: Option<String>,
}

impl Sandbox {
    /// A sandbox that no longer exists.
    pub fn gone(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            state: SandboxState::Gone,
            hydration: Hydration::Unknown,
            labels: BTreeMap::new(),
            size: None,
            auto_stop_at: None,
            data_expired_at: None,
            error: None,
        }
    }
}

/// Machine time one sandbox used this calendar month (UTC), at list price.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxCost {
    /// Billable machine-seconds, size multiplier applied.
    pub seconds: u64,
    pub dollars: f64,
    /// The start of the month counted (RFC 3339).
    pub since: Option<String>,
}

/// A request to create one sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSandbox {
    pub name: String,
    pub size: MachineSize,
    pub ttl_seconds: u64,
    pub labels: BTreeMap<String, String>,
    /// Withhold every account secret from the sandbox.
    pub no_env: bool,
    /// The provider environment to attach instead (only with `no_env` off).
    pub environment: Option<String>,
    /// Exactly-once key: a retry with the same key returns the same sandbox.
    pub idempotency_key: String,
    /// Start from this saved snapshot instead of a blank disk.
    pub from: Option<String>,
}

/// What a resume did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resumed {
    /// The provider took back the machine it kept after the stop: same
    /// machine, nothing restored, no start counted.
    pub reclaimed: bool,
    pub sandbox: Option<Sandbox>,
}

/// How to reach a sandbox over SSH once a key is authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAccess {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The server's public host key (`<type> <base64>`), to pin.
    pub host_key: String,
}

/// An accepted, asynchronous deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    pub operation_id: String,
    pub status: DeletionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStatus {
    Pending,
    Processing,
    /// Held up by a transient fault the provider retries by itself.
    Blocked,
    Completed,
}

impl DeletionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
        }
    }
}
