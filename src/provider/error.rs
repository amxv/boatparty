//! Typed provider errors.
//!
//! Callers decide what to do from [`ErrorKind`], never from message text.
//! The vendor's raw status, code, message and request id are kept only for
//! diagnostics.

use std::fmt;
use std::time::Duration;

/// A rolling start window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartWindow {
    Minute,
    Hour,
    Day,
}

impl StartWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }
}

/// Details of a refused start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartRefusal {
    /// The window that is full, when the provider named one.
    pub window: Option<StartWindow>,
    /// The window's limit.
    pub limit: Option<u64>,
    /// Why the limit applies (`trial`, `plan`, `cap`, `ceiling` at boat).
    pub cause: Option<String>,
    /// When the next start is possible, as the provider's timestamp.
    pub next_start_at: Option<String>,
    /// How long to wait before retrying.
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityReason {
    /// A fail-fast start found no ready machine; nothing started or counted.
    NoReadyMachine,
    /// The provider is out of machines for a short time.
    OutOfCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyReason {
    /// The plan's limit on sandboxes running at once.
    Plan,
    /// An organization owner capped this member lower than the plan.
    Member,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingReason {
    /// Included time used and the wallet is empty, or there is no plan.
    PaymentRequired,
    /// The organization's per-member usage cap is used.
    MemberUsageCap,
    /// The free trial's compute allowance is used.
    TrialComputeUsed,
}

/// Transient states that clear on their own; retry after a short wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyReason {
    /// Files are being mounted at the start of a resume or fork.
    Restoring,
    /// Secrets are being scrubbed during a no-env resume.
    Securing,
    /// The machine's SSH host key is not available yet.
    HostKeyUnavailable,
    /// The sandbox is still starting.
    Starting,
    /// A resume is in progress.
    Resuming,
    /// A stop holds the machine.
    Stopping,
    /// A create with the same idempotency key is still running.
    IdempotencyInProgress,
    /// A saved-snapshot save under the same name is still running.
    SaveInProgress,
    /// The machine did not complete a call but says a retry is safe.
    MachineRetryable,
}

/// Why a scoped API key refused a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDenial {
    /// The key does not carry the action (named when the provider names it).
    Action(Option<String>),
    /// The target sandbox or environment is outside the key's scope.
    Resource,
    /// The key is past its expiry.
    Expired,
    /// The key belongs to another organization or cannot act for it.
    Organization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// A start limit is full. The start did not happen and did not count.
    RateLimited(StartRefusal),
    /// Too many API requests from this address.
    RequestRateLimited { retry_after: Option<Duration> },
    /// No machine is available right now.
    AtCapacity {
        reason: CapacityReason,
        retry_after: Option<Duration>,
    },
    /// The limit on sandboxes running at once is reached.
    ConcurrencyLimit(ConcurrencyReason),
    /// The account cannot pay for more work.
    Billing {
        reason: BillingReason,
        url: Option<String>,
    },
    /// The sandbox, key or saved snapshot does not exist.
    NotFound,
    /// The sandbox's stored data was deleted after a long stop; it cannot resume.
    DataExpired,
    /// A transient state; retry after the given delay or a few seconds.
    Busy {
        reason: BusyReason,
        retry_after: Option<Duration>,
    },
    /// The sandbox has no running machine.
    NotRunning,
    /// The credential is missing or invalid.
    Unauthorized,
    /// A scoped key does not allow the request.
    ScopeForbidden(ScopeDenial),
    /// Refused for another reason (plan, size, suspended organization, environment).
    Forbidden,
    /// The request conflicts with the current state.
    Conflict,
    /// The request was malformed or names something invalid.
    InvalidRequest,
    /// A file is too large for the inline call.
    TooLarge,
    /// The provider failed internally.
    Server,
    /// The provider could not be reached or the connection failed.
    Transport,
    /// A successful answer could not be understood.
    InvalidResponse,
    /// An error code this client does not know.
    Unknown,
}

impl ErrorKind {
    /// Stable machine code used in `--json` error output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::RateLimited(_) => "start_rate_limited",
            Self::RequestRateLimited { .. } => "request_rate_limited",
            Self::AtCapacity { .. } => "provider_at_capacity",
            Self::ConcurrencyLimit(_) => "concurrency_limit",
            Self::Billing { .. } => "billing",
            Self::NotFound => "not_found",
            Self::DataExpired => "data_expired",
            Self::Busy { .. } => "provider_busy",
            Self::NotRunning => "not_running",
            Self::Unauthorized => "unauthorized",
            Self::ScopeForbidden(_) => "key_scope_forbidden",
            Self::Forbidden => "forbidden",
            Self::Conflict => "conflict",
            Self::InvalidRequest => "invalid_request",
            Self::TooLarge => "too_large",
            Self::Server => "provider_error",
            Self::Transport => "connection",
            Self::InvalidResponse => "invalid_response",
            Self::Unknown => "provider_unknown_error",
        }
    }

    /// Whether repeating the same request later can succeed without any change.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited(_)
                | Self::RequestRateLimited { .. }
                | Self::AtCapacity { .. }
                | Self::Busy { .. }
                | Self::Server
                | Self::Transport
        )
    }

    /// The provider's requested delay before a retry, when it gave one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited(refusal) => refusal.retry_after,
            Self::RequestRateLimited { retry_after }
            | Self::AtCapacity { retry_after, .. }
            | Self::Busy { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::RateLimited(refusal) => match refusal.window {
                Some(window) => format!("the provider's {} start limit is full", window.as_str()),
                None => "a provider start limit is full".to_string(),
            },
            Self::RequestRateLimited { .. } => "too many API requests from this address".into(),
            Self::AtCapacity { .. } => "the provider has no machine available right now".into(),
            Self::ConcurrencyLimit(ConcurrencyReason::Plan) => {
                "the plan's limit on sandboxes running at once is reached".into()
            }
            Self::ConcurrencyLimit(ConcurrencyReason::Member) => {
                "your organization's limit on sandboxes running at once is reached".into()
            }
            Self::Billing { reason, .. } => match reason {
                BillingReason::PaymentRequired => "the account needs a payment".into(),
                BillingReason::MemberUsageCap => "your organization usage cap is used".into(),
                BillingReason::TrialComputeUsed => "the trial's compute time is used".into(),
            },
            Self::NotFound => "not found".into(),
            Self::DataExpired => "the sandbox's stored data has expired".into(),
            Self::Busy { .. } => "the sandbox is busy; retry shortly".into(),
            Self::NotRunning => "the sandbox has no running machine".into(),
            Self::Unauthorized => "the API key is missing or invalid".into(),
            Self::ScopeForbidden(denial) => match denial {
                ScopeDenial::Action(Some(action)) => {
                    format!("the API key does not allow the action {action}")
                }
                ScopeDenial::Action(None) => "the API key does not allow this action".into(),
                ScopeDenial::Resource => "the API key does not cover this sandbox".into(),
                ScopeDenial::Expired => "the API key has expired".into(),
                ScopeDenial::Organization => "the API key cannot act for this organization".into(),
            },
            Self::Forbidden => "the provider refused the request".into(),
            Self::Conflict => "the request conflicts with the sandbox's state".into(),
            Self::InvalidRequest => "the provider rejected the request".into(),
            Self::TooLarge => "the file is too large for this call".into(),
            Self::Server => "the provider failed".into(),
            Self::Transport => "cannot reach the provider".into(),
            Self::InvalidResponse => "the provider's answer could not be read".into(),
            Self::Unknown => "the provider returned an unknown error".into(),
        }
    }
}

/// An error from a provider operation. Boxed so `Result`s stay small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError(Box<Details>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Details {
    kind: ErrorKind,
    status: Option<u16>,
    code: Option<String>,
    message: Option<String>,
    request_id: Option<String>,
}

impl ProviderError {
    pub fn new(kind: ErrorKind) -> Self {
        Self(Box::new(Details {
            kind,
            status: None,
            code: None,
            message: None,
            request_id: None,
        }))
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.0.status = Some(status);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.0.code = Some(code.into());
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        let message = message.into();
        if !message.is_empty() {
            self.0.message = Some(message);
        }
        self
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.0.request_id = request_id;
        self
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.0.kind
    }

    /// The HTTP status, when the error came from an answer.
    pub fn status(&self) -> Option<u16> {
        self.0.status
    }

    /// The provider's own error code, for diagnostics only.
    pub fn provider_code(&self) -> Option<&str> {
        self.0.code.as_deref()
    }

    /// The provider's message, for diagnostics only.
    pub fn provider_message(&self) -> Option<&str> {
        self.0.message.as_deref()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.0.request_id.as_deref()
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let details = &self.0;
        f.write_str(&details.kind.summary())?;
        let detail = details
            .message
            .as_deref()
            .filter(|message| Some(*message) != details.code.as_deref());
        match (detail, details.code.as_deref()) {
            (Some(message), Some(code)) => write!(f, ": {message} ({code})")?,
            (Some(message), None) => write!(f, ": {message}")?,
            (None, Some(code)) => write!(f, " ({code})")?,
            (None, None) => {}
        }
        if let Some(delay) = details.kind.retry_after() {
            write!(f, "; retry after {}s", delay.as_secs())?;
        }
        if let Some(request_id) = &details.request_id {
            write!(f, " [request {request_id}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}
