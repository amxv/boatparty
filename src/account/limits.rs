//! The limits that apply to one project: `[defaults]`, lowered by the
//! project's `[projects.<name>]` table, plus the account-wide `[account]`
//! limits. A project's `boatparty.toml` may ask for less, never for more.

use std::path::Path;
use std::time::Duration;

use super::config::AccountConfig;
use crate::error::AppError;
use crate::provider::MachineSize;

/// Everything one project's commands are bound by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectSettings {
    pub max_sandboxes: u32,
    pub steal_after_minutes: u32,
    pub acquire_wait_seconds: u32,
    /// The machine size new members get.
    pub machine: MachineSize,
    pub max_machine: Option<MachineSize>,
    pub lease_minutes: u32,
    pub idle_minutes: u32,
    pub heartbeat_minutes: u32,
    /// The most of this project's sandboxes running at once.
    pub max_running: Option<u32>,
    pub starts_per_hour: Option<u32>,
    pub starts_per_day: Option<u32>,
    /// The most sandboxes running at once across the account.
    pub account_max_running: Option<u32>,
    pub reserve_starts_per_hour: u32,
    pub reserve_starts_per_day: u32,
    /// `[account] paid_starts` (default true).
    pub paid_starts: bool,
}

impl ProjectSettings {
    /// The settings `account.toml` gives `project`. The default machine is
    /// lowered to the project's `max_machine` when it is larger.
    pub fn resolve(config: &AccountConfig, project: &str) -> Self {
        let defaults = &config.defaults;
        let entry = config.projects.get(project).cloned().unwrap_or_default();
        let max_machine = entry.max_machine();
        let default_machine = defaults
            .machine
            .parse::<MachineSize>()
            .unwrap_or(MachineSize::Default);
        let machine = match max_machine {
            Some(max) if default_machine > max => max,
            _ => default_machine,
        };
        let pick = |own: Option<u32>, default: u32| own.map_or(default, |own| own.min(default));
        Self {
            max_sandboxes: entry.max_sandboxes.unwrap_or(defaults.max_sandboxes),
            steal_after_minutes: defaults.steal_after_minutes,
            acquire_wait_seconds: defaults.acquire_wait_seconds,
            machine,
            max_machine,
            lease_minutes: pick(entry.lease_minutes, defaults.lease_minutes),
            idle_minutes: entry.idle_minutes.unwrap_or(defaults.idle_minutes),
            heartbeat_minutes: defaults.heartbeat_minutes,
            max_running: entry.max_running,
            starts_per_hour: entry.starts_per_hour,
            starts_per_day: entry.starts_per_day,
            account_max_running: config.account.max_running,
            reserve_starts_per_hour: config.account.reserve_starts_per_hour,
            reserve_starts_per_day: config.account.reserve_starts_per_day,
            paid_starts: config.account.paid_starts.unwrap_or(true),
        }
    }

    /// Apply what the project's `boatparty.toml` asks for. A machine larger
    /// than `max_machine` is an error naming both files.
    pub fn with_requested_machine(
        mut self,
        project: &str,
        requested: Option<MachineSize>,
        project_file: &Path,
        account_file: &Path,
    ) -> Result<Self, AppError> {
        let Some(requested) = requested else {
            return Ok(self);
        };
        if let Some(max) = self.max_machine
            && requested > max
        {
            return Err(AppError::config(format!(
                "{} asks for a {requested} machine ([sandbox] machine), but {} allows project \
                 {project} at most {max} (max_machine under [projects.{project}])",
                project_file.display(),
                account_file.display()
            ))
            .with_hint(format!(
                "ask for {max} or less in {}, or raise it with `boatparty limits set project \
                 {project} max_machine={requested}`",
                project_file.display()
            )));
        }
        self.machine = requested;
        Ok(self)
    }

    /// The auto-stop length while a lease is held.
    pub fn lease_seconds(&self) -> u64 {
        u64::from(self.lease_minutes) * 60
    }

    /// The auto-stop length after the last lease ended.
    pub fn idle_seconds(&self) -> u64 {
        u64::from(self.idle_minutes) * 60
    }

    /// How often a held lease pushes the deadline: the account's heartbeat,
    /// but at most half a (possibly shorter) project lease.
    pub fn heartbeat(&self) -> Duration {
        let heartbeat = u64::from(self.heartbeat_minutes) * 60;
        Duration::from_secs(heartbeat.min(self.lease_seconds() / 2).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> AccountConfig {
        AccountConfig::parse(text, Path::new("account.toml")).unwrap()
    }

    #[test]
    fn a_project_lowers_the_defaults_and_never_raises_the_lease() {
        let base = ProjectSettings::resolve(&config("version = 1\n"), "app");
        assert_eq!(base.lease_minutes, 5);
        assert_eq!(base.idle_minutes, 5);
        assert_eq!(base.machine, MachineSize::Default);
        assert_eq!(base.heartbeat(), Duration::from_secs(120));
        let own = ProjectSettings::resolve(
            &config(
                "version = 1\n[account]\nmax_running = 3\nreserve_starts_per_day = 9\n\
                 [projects.app]\nlease_minutes = 1\nidle_minutes = 12\nmax_running = 2\n\
                 starts_per_hour = 4\nmax_machine = \"small\"\n",
            ),
            "app",
        );
        assert_eq!(own.lease_minutes, 1);
        assert_eq!(own.idle_minutes, 12);
        // A one-minute lease gets a heartbeat of half a minute.
        assert_eq!(own.heartbeat(), Duration::from_secs(30));
        assert_eq!(own.max_running, Some(2));
        assert_eq!(own.account_max_running, Some(3));
        assert_eq!(own.reserve_starts_per_day, 9);
        assert_eq!(own.starts_per_hour, Some(4));
        // The default machine is lowered to the project's maximum.
        assert_eq!(own.machine, MachineSize::Small);
    }

    #[test]
    fn a_machine_above_max_machine_is_an_error_naming_both_files() {
        let settings = ProjectSettings::resolve(
            &config("version = 1\n[projects.app]\nmax_machine = \"default\"\n"),
            "app",
        );
        let project = Path::new("/w/boatparty.toml");
        let account = Path::new("/h/account.toml");
        let error = settings
            .with_requested_machine("app", Some(MachineSize::Large), project, account)
            .unwrap_err();
        assert_eq!(error.code(), "config");
        let message = error.message();
        assert!(message.contains("/w/boatparty.toml"), "{message}");
        assert!(message.contains("/h/account.toml"), "{message}");
        assert!(message.contains("max_machine"), "{message}");
        let smaller = settings
            .with_requested_machine("app", Some(MachineSize::Small), project, account)
            .unwrap();
        assert_eq!(smaller.machine, MachineSize::Small);
        let unchanged = settings
            .with_requested_machine("app", None, project, account)
            .unwrap();
        assert_eq!(unchanged.machine, MachineSize::Default);
    }
}
