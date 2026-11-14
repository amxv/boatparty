//! `account.toml`: provider settings and limits shared by every project.
//!
//! Created with documented defaults the first time a command needs it.
//! Unknown keys are an error so a typo never silently changes nothing.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::{MachineSize, PROVIDER_NAME};

pub const ACCOUNT_FILE_VERSION: u32 = 1;

/// The file written on first use. Its values must equal the serde defaults.
pub const DEFAULT_ACCOUNT_TOML: &str = r#"# boatparty account settings, shared by every project on this machine.
# boatparty created this file with its defaults. Edit it freely: values here
# apply to every project, and unknown keys are an error. The API key is not
# here; it lives in credentials.toml next to this file.
version = 1

[provider]
# The sandbox provider. "boat" is the only one.
name = "boat"
# Use another API base URL instead of the provider's public API
# (https://, or http:// to 127.0.0.1, localhost or [::1] only).
# api_url = "https://example.invalid/api/v1"

[defaults]
# Defaults for every project. A project's boatparty.toml can ask for less.
# Machine size for new sandboxes: small, default, large or xlarge.
machine = "default"
# The most sandboxes one project's pool may own.
max_sandboxes = 8
# An idle sandbox may be handed to another workspace after this many minutes.
steal_after_minutes = 30
# While a command uses a sandbox, keep its auto-stop this many minutes ahead.
lease_minutes = 15
# After the last command ends, the sandbox stops after this many minutes.
idle_minutes = 5
# How often, in minutes, a running command pushes the auto-stop forward.
heartbeat_minutes = 5
# How long to wait for a free sandbox before failing, in seconds.
acquire_wait_seconds = 900

# Projects registered by `boatparty init`, one [projects.<name>] table each,
# appended below.
"#;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub version: u32,
    #[serde(default)]
    pub provider: ProviderSettings,
    #[serde(default)]
    pub defaults: Defaults,
    /// Projects registered by `boatparty init`, by project name.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
}

/// One registered project. Per-project limits are added here later.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectEntry {}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    #[serde(default = "default_provider_name")]
    pub name: String,
    #[serde(default)]
    pub api_url: Option<String>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            name: default_provider_name(),
            api_url: None,
        }
    }
}

fn default_provider_name() -> String {
    PROVIDER_NAME.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub machine: String,
    pub max_sandboxes: u32,
    pub steal_after_minutes: u32,
    pub lease_minutes: u32,
    pub idle_minutes: u32,
    pub heartbeat_minutes: u32,
    pub acquire_wait_seconds: u32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            machine: MachineSize::Default.as_str().to_string(),
            max_sandboxes: 8,
            steal_after_minutes: 30,
            lease_minutes: 15,
            idle_minutes: 5,
            heartbeat_minutes: 5,
            acquire_wait_seconds: 900,
        }
    }
}

impl AccountConfig {
    pub fn parse(text: &str, path: &Path) -> Result<Self, AppError> {
        let config: Self = toml::from_str(text)
            .map_err(|error| AppError::config(format!("{}: {error}", path.display())))?;
        config.validate(path)?;
        Ok(config)
    }

    fn validate(&self, path: &Path) -> Result<(), AppError> {
        let fail =
            |message: String| Err(AppError::config(format!("{}: {message}", path.display())));
        if self.version != ACCOUNT_FILE_VERSION {
            return fail(format!(
                "version {} is not supported (expected {ACCOUNT_FILE_VERSION})",
                self.version
            ));
        }
        if self.provider.name != PROVIDER_NAME {
            return fail(format!(
                "provider.name {:?} is not supported (expected {PROVIDER_NAME:?})",
                self.provider.name
            ));
        }
        if let Err(message) = self.defaults.machine.parse::<MachineSize>() {
            return fail(format!("defaults.machine: {message}"));
        }
        let positive = [
            ("max_sandboxes", self.defaults.max_sandboxes),
            ("lease_minutes", self.defaults.lease_minutes),
            ("idle_minutes", self.defaults.idle_minutes),
            ("heartbeat_minutes", self.defaults.heartbeat_minutes),
        ];
        for (name, value) in positive {
            if value == 0 {
                return fail(format!("defaults.{name} must be at least 1"));
            }
        }
        for name in self.projects.keys() {
            if let Err(message) = crate::project::validate_name(name) {
                return fail(format!("projects: {message}"));
            }
        }
        if self.defaults.heartbeat_minutes >= self.defaults.lease_minutes {
            return fail(format!(
                "defaults.heartbeat_minutes ({}) must be less than defaults.lease_minutes ({})",
                self.defaults.heartbeat_minutes, self.defaults.lease_minutes
            ));
        }
        Ok(())
    }
}

/// The account configuration and whether this call created the file.
pub struct LoadedAccount {
    pub config: AccountConfig,
    pub path: PathBuf,
    pub created: bool,
}

/// Read `account.toml`, creating it with the defaults when it does not exist.
pub fn load_or_create(paths: &Paths) -> Result<LoadedAccount, AppError> {
    let path = paths.account_file();
    let mut created = false;
    if !path.exists() {
        created = create_default(&path)?;
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| AppError::io(format!("cannot read {}", path.display()), error))?;
    let config = AccountConfig::parse(&text, &path)?;
    Ok(LoadedAccount {
        config,
        path,
        created,
    })
}

/// Register a project in `account.toml` by appending an empty
/// `[projects.<name>]` table, leaving the rest of the file untouched.
/// Returns false when it was already registered.
pub fn register_project(paths: &Paths, name: &str) -> Result<bool, AppError> {
    crate::project::validate_name(name).map_err(AppError::config)?;
    let loaded = load_or_create(paths)?;
    if loaded.config.projects.contains_key(name) {
        return Ok(false);
    }
    let path = &loaded.path;
    let text = fs::read_to_string(path)
        .map_err(|error| AppError::io(format!("cannot read {}", path.display()), error))?;
    let separator = if text.ends_with('\n') || text.is_empty() {
        "\n"
    } else {
        "\n\n"
    };
    let addition = format!("{separator}[projects.{name}]\n");
    let mut updated = text.clone();
    updated.push_str(&addition);
    // Refuse to write a file that would no longer parse.
    AccountConfig::parse(&updated, path)?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| AppError::io(format!("cannot open {}", path.display()), error))?;
    file.write_all(addition.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))?;
    Ok(true)
}

/// Write the default file unless another process won the race.
fn create_default(path: &Path) -> Result<bool, AppError> {
    let dir = path
        .parent()
        .expect("the account file always has a parent directory");
    fs::create_dir_all(dir)
        .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => {
            return Err(AppError::io(
                format!("cannot create {}", path.display()),
                error,
            ));
        }
    };
    file.write_all(DEFAULT_ACCOUNT_TOML.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<AccountConfig, AppError> {
        AccountConfig::parse(text, Path::new("account.toml"))
    }

    #[test]
    fn the_default_file_parses_to_the_defaults() {
        let config = parse(DEFAULT_ACCOUNT_TOML).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.provider, ProviderSettings::default());
        assert_eq!(config.defaults, Defaults::default());
    }

    #[test]
    fn a_minimal_file_uses_the_defaults() {
        let config = parse("version = 1\n").unwrap();
        assert_eq!(config.defaults, Defaults::default());
        assert_eq!(config.provider.api_url, None);
    }

    #[test]
    fn unknown_keys_are_errors_that_name_the_key() {
        let error = parse("version = 1\n[defaults]\nidle_minuets = 3\n").unwrap_err();
        assert!(
            error.message().contains("idle_minuets"),
            "{}",
            error.message()
        );
        assert!(parse("version = 1\nsurprise = true\n").is_err());
    }

    #[test]
    fn projects_are_registered_tables() {
        let config = parse("version = 1\n[projects.app]\n[projects.gg-desktop]\n").unwrap();
        assert_eq!(
            config.projects.keys().collect::<Vec<_>>(),
            ["app", "gg-desktop"]
        );
        assert!(parse("version = 1\n[projects.App]\n").is_err());
        assert!(parse("version = 1\n[projects.app]\nsurprise = 1\n").is_err());
    }

    #[test]
    fn registering_appends_and_keeps_comments() {
        let root = std::env::temp_dir().join(format!(
            "boatparty-account-test-{}-register",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = std::ffi::OsString::from(root.as_os_str());
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        assert!(register_project(&paths, "app").unwrap());
        assert!(!register_project(&paths, "app").unwrap());
        assert!(register_project(&paths, "other").unwrap());
        let text = fs::read_to_string(paths.account_file()).unwrap();
        assert!(text.starts_with(DEFAULT_ACCOUNT_TOML));
        assert!(text.ends_with("[projects.app]\n\n[projects.other]\n"));
        let config = AccountConfig::parse(&text, Path::new("account.toml")).unwrap();
        assert_eq!(config.projects.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(parse("version = 2\n").is_err());
        assert!(parse("version = 1\n[provider]\nname = \"other\"\n").is_err());
        assert!(parse("version = 1\n[defaults]\nmachine = \"huge\"\n").is_err());
        assert!(parse("version = 1\n[defaults]\nidle_minutes = 0\n").is_err());
        assert!(
            parse("version = 1\n[defaults]\nheartbeat_minutes = 15\nlease_minutes = 15\n").is_err()
        );
    }
}
