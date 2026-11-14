//! A small synchronous client for boat's HTTP API.
//!
//! The API key travels only in the `Authorization` header of requests made
//! here. It is never formatted into a URL, an error or a log line.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::errors::map_error;
use crate::account::ApiKey;
use crate::error::AppError;
use crate::provider::{ErrorKind, ProviderError};

pub const DEFAULT_API_URL: &str = "https://boat.dev/api/v1";
const USER_AGENT: &str = concat!("boatparty/", env!("CARGO_PKG_VERSION"));
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

/// A raw answer, before it is judged a success or an error.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The typed error for a non-success answer.
    pub fn error(&self) -> ProviderError {
        map_error(self.status, self.retry_after.as_deref(), &self.body)
    }

    /// Decode a success body, or map the error answer.
    pub fn json<T: DeserializeOwned>(self) -> Result<T, ProviderError> {
        if !self.is_success() {
            return Err(self.error());
        }
        serde_json::from_slice(&self.body).map_err(|error| {
            ProviderError::new(ErrorKind::InvalidResponse)
                .with_status(self.status)
                .with_message(error.to_string())
        })
    }
}

#[derive(Clone)]
pub struct Http {
    agent: ureq::Agent,
    base: String,
    key: ApiKey,
}

impl Http {
    pub fn new(api_url: Option<&str>, key: ApiKey) -> Result<Self, AppError> {
        let base = validate_base(api_url.unwrap_or(DEFAULT_API_URL))?;
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(USER_AGENT)
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            base,
            key,
        })
    }

    /// Send one request. Only connection failures are errors here; any HTTP
    /// status comes back as an [`Answer`].
    pub fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        extra_header: Option<(&str, &str)>,
    ) -> Result<Answer, ProviderError> {
        let url = format!("{}{}", self.base, path);
        let auth = format!("Bearer {}", self.key.expose());
        let result = match method {
            Method::Get | Method::Delete => {
                let mut request = if method == Method::Get {
                    self.agent.get(&url)
                } else {
                    self.agent.delete(&url)
                };
                request = request
                    .header("Authorization", &auth)
                    .header("Accept", "application/json");
                if let Some((name, value)) = extra_header {
                    request = request.header(name, value);
                }
                request.call()
            }
            Method::Post | Method::Patch => {
                let mut request = if method == Method::Post {
                    self.agent.post(&url)
                } else {
                    self.agent.patch(&url)
                };
                request = request
                    .header("Authorization", &auth)
                    .header("Accept", "application/json");
                if let Some((name, value)) = extra_header {
                    request = request.header(name, value);
                }
                match body {
                    Some(body) => request.send_json(body),
                    None => request.send_empty(),
                }
            }
        };
        let mut response = result.map_err(transport_error)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_vec()
            .map_err(transport_error)?;
        Ok(Answer {
            status,
            retry_after,
            body,
        })
    }
}

fn transport_error(error: ureq::Error) -> ProviderError {
    ProviderError::new(ErrorKind::Transport).with_message(error.to_string())
}

/// Accept HTTPS, or plain HTTP to a loopback address only (local fakes), so
/// the key is never sent in the clear over a network.
fn validate_base(url: &str) -> Result<String, AppError> {
    let trimmed = url.trim().trim_end_matches('/');
    let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"];
    let allowed = trimmed.starts_with("https://")
        || loopback.iter().any(|prefix| {
            trimmed.strip_prefix(prefix).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
            })
        });
    if !allowed {
        return Err(AppError::config(format!(
            "provider API URL must use https:// (plain http is allowed only for 127.0.0.1, localhost or [::1]): {trimmed}"
        )));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_base;

    #[test]
    fn https_and_loopback_http_are_accepted() {
        assert_eq!(
            validate_base("https://boat.dev/api/v1/").unwrap(),
            "https://boat.dev/api/v1"
        );
        assert!(validate_base("http://127.0.0.1:4100/api/v1").is_ok());
        assert!(validate_base("http://localhost/api").is_ok());
        assert!(validate_base("http://[::1]:9/api").is_ok());
    }

    #[test]
    fn plain_http_elsewhere_is_refused() {
        assert!(validate_base("http://boat.dev/api/v1").is_err());
        assert!(validate_base("http://127.0.0.1.evil.example/api").is_err());
        assert!(validate_base("ftp://boat.dev").is_err());
    }
}
