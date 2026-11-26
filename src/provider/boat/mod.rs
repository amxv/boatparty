//! boat (https://boat.dev), the only provider.

mod errors;
mod files;
mod hosting;
mod http;
mod limits;
mod sandboxes;
mod scope;
mod snapshots;

use std::path::Path;

use self::http::{Http, Method};
use self::limits::LimitsBody;
use crate::account::ApiKey;
use crate::error::AppError;
use crate::provider::{
    AccountLimits, CreateSandbox, CredentialCheck, CredentialHelp, Deletion, HostPort, HostedPort,
    Provider, ProviderError, Resumed, Sandbox, SandboxCost, SavedSnapshot, SnapshotAllowance,
    SshAccess,
};

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

    fn list_by_label(&self, key: &str, value: &str) -> Result<Vec<Sandbox>, ProviderError> {
        sandboxes::list_by_label(&self.http, key, value)
    }

    fn list_by_label_with_cost(
        &self,
        key: &str,
        value: &str,
    ) -> Result<Vec<(Sandbox, Option<SandboxCost>)>, ProviderError> {
        sandboxes::list_by_label_with_cost(&self.http, key, value)
    }

    fn get(&self, id: &str) -> Result<Sandbox, ProviderError> {
        sandboxes::get(&self.http, id)
    }

    fn create(&self, request: &CreateSandbox) -> Result<Sandbox, ProviderError> {
        sandboxes::create(&self.http, request)
    }

    fn resume(&self, id: &str, ttl_seconds: u64) -> Result<Resumed, ProviderError> {
        sandboxes::resume(&self.http, id, ttl_seconds)
    }

    fn stop(&self, id: &str) -> Result<(), ProviderError> {
        sandboxes::stop(&self.http, id, false)
    }

    fn force_stop(&self, id: &str) -> Result<(), ProviderError> {
        sandboxes::stop(&self.http, id, true)
    }

    fn set_ttl(&self, id: &str, ttl_seconds: u64) -> Result<(), ProviderError> {
        sandboxes::set_ttl(&self.http, id, ttl_seconds)
    }

    fn delete(&self, id: &str) -> Result<Deletion, ProviderError> {
        sandboxes::delete(&self.http, id)
    }

    fn deletion(&self, operation_id: &str) -> Result<Deletion, ProviderError> {
        sandboxes::deletion(&self.http, operation_id)
    }

    fn authorize_ssh_key(&self, id: &str, public_key: &str) -> Result<SshAccess, ProviderError> {
        sandboxes::authorize_ssh_key(&self.http, id, public_key)
    }

    fn upload_file(&self, id: &str, remote_path: &str, file: &Path) -> Result<(), ProviderError> {
        files::upload_file(&self.http, id, remote_path, file)
    }

    fn download_file(
        &self,
        id: &str,
        remote_path: &str,
        file: &Path,
    ) -> Result<u64, ProviderError> {
        files::download_file(&self.http, id, remote_path, file)
    }

    fn save_snapshot(&self, sandbox_id: &str, name: &str) -> Result<SavedSnapshot, ProviderError> {
        snapshots::save(&self.http, sandbox_id, name)
    }

    fn saved_snapshot(&self, name: &str) -> Result<Option<SavedSnapshot>, ProviderError> {
        snapshots::get(&self.http, name)
    }

    fn saved_snapshots(
        &self,
        name: &str,
    ) -> Result<(Vec<SavedSnapshot>, Option<SnapshotAllowance>), ProviderError> {
        snapshots::list(&self.http, name)
    }

    fn delete_saved_snapshot(&self, name: &str) -> Result<bool, ProviderError> {
        snapshots::delete(&self.http, name)
    }

    fn desktop_link(&self, id: &str) -> Result<String, ProviderError> {
        hosting::desktop_link(&self.http, id)
    }

    fn host_port(&self, id: &str, request: &HostPort) -> Result<HostedPort, ProviderError> {
        hosting::host_port(&self.http, id, request)
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
