//! The application error boundary and exit statuses.
//!
//! Every failure that is boatparty's own (configuration, credentials,
//! provider, connection, limits) exits with [`EXIT_TOOL_FAILURE`] so a caller
//! can tell a failing remote command from a failing sandbox. Each error
//! carries a stable machine code for `--json` output.

use std::fmt;

use crate::provider::ProviderError;

/// boatparty itself failed.
pub const EXIT_TOOL_FAILURE: u8 = 125;
/// Command-line usage error (clap's own convention).
pub const EXIT_USAGE: u8 = 2;

#[derive(Debug)]
pub struct AppError {
    code: &'static str,
    message: String,
}

impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Invalid or unreadable configuration or environment.
    pub fn config(message: impl Into<String>) -> Self {
        Self::new("config", message)
    }

    /// No usable credential, or a credential that cannot be stored.
    pub fn credentials(message: impl Into<String>) -> Self {
        Self::new("credentials", message)
    }

    /// A local file operation failed.
    pub fn io(context: impl fmt::Display, error: std::io::Error) -> Self {
        Self::new("io", format!("{context}: {error}"))
    }

    /// Append advice to the message.
    pub fn with_hint(mut self, hint: impl fmt::Display) -> Self {
        self.message = format!("{}; {hint}", self.message);
        self
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn exit_code(&self) -> u8 {
        EXIT_TOOL_FAILURE
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl From<ProviderError> for AppError {
    fn from(error: ProviderError) -> Self {
        Self::new(error.kind().code(), error.to_string())
    }
}
