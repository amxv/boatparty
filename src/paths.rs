//! Where boatparty keeps its files on the developer machine.
//!
//! Two roots: a configuration root (human and agent editable) and a state
//! root (machine owned). `BOATPARTY_HOME`, when set, replaces both, with the
//! configuration under `<home>/config` and the state under `<home>/state`.
//! Otherwise `XDG_CONFIG_HOME` and `XDG_STATE_HOME` are honored, falling back
//! to `~/.config/boatparty` and `~/.local/state/boatparty` on every platform.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::AppError;

pub const HOME_ENV: &str = "BOATPARTY_HOME";
/// Replaces the SSH control socket directory (an absolute path short enough
/// for a socket path); for tests.
pub const CONTROL_DIR_ENV: &str = "BOATPARTY_CONTROL_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    state_dir: PathBuf,
    control_dir: Option<PathBuf>,
}

impl Paths {
    /// Resolve the roots from the process environment.
    pub fn from_env() -> Result<Self, AppError> {
        Self::resolve(|name| std::env::var_os(name))
    }

    /// Resolve the roots from an arbitrary variable lookup (used by tests).
    pub fn resolve(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, AppError> {
        let control_dir = non_empty(var(CONTROL_DIR_ENV))
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute());
        if let Some(home) = non_empty(var(HOME_ENV)) {
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err(AppError::config(format!(
                    "{HOME_ENV} must be an absolute path, got {}",
                    home.display()
                )));
            }
            return Ok(Self {
                config_dir: home.join("config"),
                state_dir: home.join("state"),
                control_dir,
            });
        }

        let user_home = non_empty(var("HOME"));
        #[cfg(windows)]
        let user_home = non_empty(var("USERPROFILE")).or(user_home);
        let user_home = user_home.map(PathBuf::from);
        let base = |xdg: &str, fallback: &[&str]| -> Result<PathBuf, AppError> {
            if let Some(dir) = non_empty(var(xdg)).map(PathBuf::from)
                && dir.is_absolute()
            {
                return Ok(dir);
            }
            let Some(home) = user_home.as_ref().filter(|home| home.is_absolute()) else {
                return Err(AppError::config(format!(
                    "cannot find the home directory: set HOME, {xdg} or {HOME_ENV}"
                )));
            };
            Ok(fallback
                .iter()
                .fold(home.clone(), |path, part| path.join(part)))
        };

        Ok(Self {
            config_dir: base("XDG_CONFIG_HOME", &[".config"])?.join("boatparty"),
            state_dir: base("XDG_STATE_HOME", &[".local", "state"])?.join("boatparty"),
            control_dir,
        })
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Provider settings, account-wide limits and per-project limits.
    pub fn account_file(&self) -> PathBuf {
        self.config_dir.join("account.toml")
    }

    /// The boat API key, mode 0600.
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }

    /// The tool's own SSH key pair (`id_ed25519` and `id_ed25519.pub`).
    pub fn ssh_key_file(&self) -> PathBuf {
        self.config_dir.join("ssh").join("id_ed25519")
    }

    /// User-level presets.
    pub fn user_presets_dir(&self) -> PathBuf {
        self.config_dir.join("presets")
    }

    /// Pools, leases, affinity, bases, the start ledger and the limits cache.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    /// The advisory lock guarding `state.json`.
    pub fn state_lock_file(&self) -> PathBuf {
        self.state_dir.join("state.lock")
    }

    /// One record per finished invocation.
    pub fn runs_file(&self) -> PathBuf {
        self.state_dir.join("runs.jsonl")
    }

    /// Generated OpenSSH configs and pinned host keys, one pair per sandbox.
    pub fn ssh_state_dir(&self) -> PathBuf {
        self.state_dir.join("ssh")
    }

    /// The generated OpenSSH config for one sandbox.
    pub fn ssh_config_file(&self, sandbox_id: &str) -> PathBuf {
        self.ssh_state_dir().join(format!("{sandbox_id}.conf"))
    }

    /// The pinned host key for one sandbox.
    pub fn known_hosts_file(&self, sandbox_id: &str) -> PathBuf {
        self.ssh_state_dir()
            .join(format!("{sandbox_id}.known_hosts"))
    }

    /// SSH control sockets. Short and outside the state root because macOS
    /// limits a socket path to 104 bytes.
    #[cfg(unix)]
    pub fn control_dir(&self) -> PathBuf {
        self.control_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/boatparty-{}", crate::system::uid())))
    }

    /// Where `boatparty skill` writes the bundled skill.
    pub fn skill_dir(&self) -> PathBuf {
        self.state_dir.join("skill").join("boatparty")
    }
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(vars: &[(&str, &str)]) -> Result<Paths, AppError> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(key, value)| (key.to_string(), OsString::from(value)))
            .collect();
        Paths::resolve(|name| vars.get(name).cloned())
    }

    #[cfg(unix)]
    #[test]
    fn defaults_follow_home() {
        let paths = resolve(&[("HOME", "/home/u")]).unwrap();
        assert_eq!(paths.config_dir(), Path::new("/home/u/.config/boatparty"));
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/u/.local/state/boatparty")
        );
        assert_eq!(
            paths.credentials_file(),
            Path::new("/home/u/.config/boatparty/credentials.toml")
        );
        assert_eq!(
            paths.state_file(),
            Path::new("/home/u/.local/state/boatparty/state.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn xdg_variables_are_honored() {
        let paths = resolve(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_STATE_HOME", "/st"),
        ])
        .unwrap();
        assert_eq!(paths.config_dir(), Path::new("/cfg/boatparty"));
        assert_eq!(paths.state_dir(), Path::new("/st/boatparty"));
    }

    #[cfg(unix)]
    #[test]
    fn relative_or_empty_xdg_values_are_ignored() {
        let paths = resolve(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "relative"),
            ("XDG_STATE_HOME", ""),
        ])
        .unwrap();
        assert_eq!(paths.config_dir(), Path::new("/home/u/.config/boatparty"));
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/u/.local/state/boatparty")
        );
    }

    #[cfg(unix)]
    #[test]
    fn boatparty_home_replaces_both_roots() {
        let paths = resolve(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "/cfg"),
            (HOME_ENV, "/tmp/bp"),
        ])
        .unwrap();
        assert_eq!(paths.config_dir(), Path::new("/tmp/bp/config"));
        assert_eq!(paths.state_dir(), Path::new("/tmp/bp/state"));
    }

    #[test]
    fn relative_boatparty_home_is_an_error() {
        assert!(resolve(&[(HOME_ENV, "bp")]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn defaults_follow_windows_profile() {
        let paths = resolve(&[("USERPROFILE", "C:/Users/A B")]).unwrap();
        assert_eq!(
            paths.config_dir(),
            Path::new("C:/Users/A B/.config/boatparty")
        );
        assert_eq!(
            paths.state_dir(),
            Path::new("C:/Users/A B/.local/state/boatparty")
        );
    }

    #[test]
    fn missing_home_is_an_error() {
        assert!(resolve(&[]).is_err());
    }
}
