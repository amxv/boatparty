//! The boundary to the sandbox vendor.
//!
//! Everything vendor-specific (URLs, state names, error codes, permission
//! names) lives below this module. The rest of boatparty sees the
//! [`Provider`] trait, the plain data types here and [`ProviderError`].
//! boat is the only implementation; later phases add operations to the
//! trait, not error plumbing.

mod boat;
pub mod error;
pub mod sandbox;

use std::fmt;
use std::str::FromStr;

pub use error::{
    BillingReason, BusyReason, CapacityReason, ConcurrencyReason, ErrorKind, ProviderError,
    ScopeDenial, StartRefusal, StartWindow,
};
pub use sandbox::{
    CreateSandbox, Deletion, DeletionStatus, Hydration, Resumed, Sandbox, SandboxState, SshAccess,
};

use crate::account::ApiKey;
use crate::error::AppError;

/// Overrides the provider's API base URL (tests point it at a local fake).
pub const API_URL_ENV: &str = "BOATPARTY_BOAT_API_URL";

/// One sandbox vendor.
pub trait Provider {
    /// Account-wide limits and usage.
    fn limits(&self) -> Result<AccountLimits, ProviderError>;

    /// Check that the credential is valid and allows everything boatparty does.
    fn check_credential(&self) -> Result<CredentialCheck, ProviderError>;

    /// Every sandbox carrying the label `key=value`.
    fn list_by_label(&self, key: &str, value: &str) -> Result<Vec<Sandbox>, ProviderError>;

    /// One sandbox. A sandbox that does not exist comes back as
    /// [`SandboxState::Gone`], not as an error.
    fn get(&self, id: &str) -> Result<Sandbox, ProviderError>;

    /// Create a sandbox. Safe to repeat with the same idempotency key.
    fn create(&self, request: &CreateSandbox) -> Result<Sandbox, ProviderError>;

    /// Start a stopped sandbox again with this auto-stop length.
    fn resume(&self, id: &str, ttl_seconds: u64) -> Result<Resumed, ProviderError>;

    /// Stop a sandbox, keeping its disk. Returns once the stop is accepted.
    fn stop(&self, id: &str) -> Result<(), ProviderError>;

    /// Set the auto-stop deadline to now plus `ttl_seconds`.
    fn set_ttl(&self, id: &str, ttl_seconds: u64) -> Result<(), ProviderError>;

    /// Delete a sandbox and its data. Asynchronous: poll [`Provider::deletion`].
    fn delete(&self, id: &str) -> Result<Deletion, ProviderError>;

    /// The progress of an accepted deletion.
    fn deletion(&self, operation_id: &str) -> Result<Deletion, ProviderError>;

    /// Authorize an OpenSSH public key on a running sandbox and say how to
    /// reach it, including the host key to pin.
    fn authorize_ssh_key(&self, id: &str, public_key: &str) -> Result<SshAccess, ProviderError>;
}

/// Open the provider described by the account settings with this key.
pub fn connect(api_url: Option<&str>, key: ApiKey) -> Result<Box<dyn Provider>, AppError> {
    let env_url = std::env::var(API_URL_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    let url = env_url.as_deref().or(api_url);
    Ok(Box::new(boat::BoatProvider::new(url, key)?))
}

/// What the user should run to get a credential with the right scope.
pub fn credential_help() -> CredentialHelp {
    boat::credential_help()
}

/// Check that a pasted credential has the provider's key format, without
/// echoing it. Returns a reason when it does not.
pub fn check_key_format(key: &str) -> Result<(), String> {
    boat::check_key_format(key)
}

/// The name of the only provider, as written in `account.toml`.
pub const PROVIDER_NAME: &str = "boat";

/// Sandbox machine sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MachineSize {
    Small,
    Default,
    Large,
    XLarge,
}

impl MachineSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Default, Self::Large, Self::XLarge];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Default => "default",
            Self::Large => "large",
            Self::XLarge => "xlarge",
        }
    }
}

impl fmt::Display for MachineSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MachineSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|size| size.as_str() == value)
            .ok_or_else(|| {
                format!("unknown machine size {value:?}; expected small, default, large or xlarge")
            })
    }
}

/// Usage of one rolling start window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowUsage {
    pub limit: u64,
    pub used: u64,
    pub remaining: u64,
}

/// Starts used and left in each rolling window. A `None` window is unlimited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartUsage {
    pub unlimited: bool,
    pub minute: Option<WindowUsage>,
    pub hour: Option<WindowUsage>,
    pub day: Option<WindowUsage>,
}

/// VM time included in the plan for the current billing period.
#[derive(Debug, Clone, PartialEq)]
pub struct IncludedTime {
    /// Hours of a `default` machine included this period.
    pub vm_hours: f64,
    /// Hours of a `default` machine left this period.
    pub vm_hours_remaining: f64,
    /// When the included amounts reset (RFC 3339), if known.
    pub period_end: Option<String>,
}

/// Whether the account can create a machine of one size now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeAvailability {
    pub size: MachineSize,
    pub available: bool,
    pub reason: Option<String>,
}

/// Prices the account pays past its plan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OveragePrices {
    /// Dollars per hour of a `default` machine.
    pub vm_hour_dollars: Option<f64>,
    /// Dollars per day for each saved snapshot above the included count.
    pub saved_snapshot_day_dollars: Option<f64>,
}

/// Account-wide limits and usage.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountLimits {
    pub can_start: bool,
    pub start_blocked_reason: Option<String>,
    pub starts: StartUsage,
    pub running_sandboxes: u64,
    pub max_running_sandboxes: u64,
    pub included: Option<IncludedTime>,
    pub sizes: Vec<SizeAvailability>,
    pub overage: OveragePrices,
    /// What the next start costs now: 0 inside every limit, `None` when it would be refused.
    pub next_start_dollars: Option<f64>,
}

/// The outcome of checking a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialCheck {
    /// Valid and allows every permission boatparty needs.
    Complete,
    /// Valid, but these permissions are missing.
    MissingPermissions(Vec<String>),
    /// Valid, but limited to particular sandboxes or environments.
    RestrictedResources,
    /// Rejected by the provider.
    Invalid,
    /// Past its expiry.
    Expired,
}

/// How to obtain a credential with the right scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialHelp {
    /// Every permission boatparty needs.
    pub permissions: Vec<&'static str>,
    /// A command that creates such a credential and prints it.
    pub create_command: String,
    /// A command that creates such a credential and stores it with `boatparty login`.
    pub create_and_login_command: String,
}
