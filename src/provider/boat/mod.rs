//! boat (https://boat.dev), the only provider.

mod errors;
mod http;
mod limits;
mod scope;

use self::http::{Http, Method};
use self::limits::LimitsBody;
use crate::account::ApiKey;
use crate::error::AppError;
use crate::provider::{AccountLimits, CredentialCheck, CredentialHelp, Provider, ProviderError};

pub struct BoatProvider {
    http: Http,
}

impl BoatProvider {
    pub fn new(api_url: Option<&str>, key: ApiKey) -> Result<Self, AppError> {
        Ok(Self {
            http: Http::new(api_url, key)?,
        })
    }
}

impl Provider for BoatProvider {
    fn limits(&self) -> Result<AccountLimits, ProviderError> {
        let body: LimitsBody = self.http.send(Method::Get, "/limits", None, None)?.json()?;
        Ok(body.into())
    }

    fn check_credential(&self) -> Result<CredentialCheck, ProviderError> {
        scope::check(&self.http)
    }
}

pub fn credential_help() -> CredentialHelp {
    scope::help()
}

/// boat keys start with `boat_` (older keys with `box_`) and are one token.
pub fn check_key_format(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("the key is empty".into());
    }
    if key.len() > 512 {
        return Err("the key is longer than any boat API key".into());
    }
    if key
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || !c.is_ascii())
    {
        return Err("the key must be a single token without spaces".into());
    }
    if !(key.starts_with("boat_") || key.starts_with("box_")) {
        return Err("boat API keys start with boat_ (or box_ for older keys)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_key_format;

    #[test]
    fn key_format() {
        assert!(check_key_format("boat_abcDEF123").is_ok());
        assert!(check_key_format("box_abc").is_ok());
        assert!(check_key_format("").is_err());
        assert!(check_key_format("boat_abc def").is_err());
        assert!(check_key_format("sk-ant-abc").is_err());
        assert!(check_key_format(&format!("boat_{}", "a".repeat(600))).is_err());
    }
}
