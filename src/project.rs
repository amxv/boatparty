//! The workspace (one checkout, identified by its Git toplevel) and its
//! project file, `boatparty.toml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::presets::PresetDirs;
use crate::config::{self, Config};
use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::MachineSize;
use crate::sync::History;
use crate::transport::ssh::shell_quote;

pub const PROJECT_FILE: &str = "boatparty.toml";
pub const PROJECT_FILE_VERSION: u32 = 1;
const NAME_MAX: usize = 41;

/// One checkout of a project on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The absolute path of the Git toplevel.
    pub root: PathBuf,
}

impl Workspace {
    /// The workspace containing `dir`: its Git toplevel.
    pub fn discover(dir: &Path) -> Result<Self, AppError> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                AppError::io("cannot run git", error)
                    .with_hint("install Git and run `boatparty doctor`")
            })?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || text.is_empty() {
            return Err(AppError::new(
                "workspace",
                format!(
                    "{} is not inside a Git repository; boatparty works on Git checkouts only",
                    dir.display()
                ),
            ));
        }
        let root = fs::canonicalize(&text).unwrap_or_else(|_| PathBuf::from(&text));
        Ok(Self { root })
    }

    /// The workspace of the current directory.
    pub fn current() -> Result<Self, AppError> {
        let cwd = std::env::current_dir()
            .map_err(|error| AppError::io("cannot read the current directory", error))?;
        Self::discover(&cwd)
    }

    /// The workspace key stored as a member's affinity.
    pub fn key(&self) -> String {
        self.root.display().to_string()
    }

    pub fn project_file(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    /// Read this workspace's `boatparty.toml` and apply its presets, for the
    /// commands that use a sandbox.
    pub fn load_project(&self, paths: &Paths) -> Result<ProjectConfig, AppError> {
        let project = ProjectConfig {
            resolved: config::load(&self.root, paths)?,
        };
        project.require_supported()?;
        Ok(project)
    }
}

/// A project's effective configuration: `boatparty.toml` with its presets
/// applied.
#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub resolved: config::Resolved,
}

impl ProjectConfig {
    /// Resolve a project file's text with the built-in presets only.
    pub fn parse(text: &str, path: &Path) -> Result<Self, AppError> {
        let dirs = PresetDirs {
            user: PathBuf::from("/nonexistent/boatparty/presets"),
            project: None,
        };
        Ok(Self {
            resolved: config::resolve(text, path, &dirs)?,
        })
    }

    pub fn config(&self) -> &Config {
        &self.resolved.config
    }

    pub fn name(&self) -> &str {
        &self.config().project.name
    }

    /// The machine size `[sandbox] machine` asks for, if any.
    pub fn requested_machine(&self) -> Option<MachineSize> {
        self.config()
            .sandbox
            .machine
            .as_deref()
            .and_then(|value| value.parse().ok())
    }

    /// The boat environment `[sandbox] environment` names, if any. Without
    /// one, new sandboxes get none of the account's secrets.
    pub fn environment(&self) -> Option<&str> {
        self.config().sandbox.environment.as_deref()
    }

    /// How seeding from the GitHub remote is set up for this project.
    pub fn seed_config(&self) -> crate::sync::SeedConfig {
        let sync = &self.config().sync;
        crate::sync::SeedConfig::from_env(sync.seed, sync.github_token)
    }

    /// How much Git history a sync sends.
    pub fn history(&self) -> History {
        History::parse(&self.config().sync.history).unwrap_or_default()
    }

    /// Whether a command's file changes are applied locally.
    pub fn copy_back(&self) -> bool {
        self.config().sync.copy_back
    }

    /// Settings that are stored but that this version cannot honor yet.
    fn require_supported(&self) -> Result<(), AppError> {
        let remote = &self.config().sandbox.remote_path;
        let default = config::default_remote_path(self.name());
        if remote.trim_end_matches('/') != default {
            return Err(AppError::config(format!(
                "{}: sandbox.remote_path = {remote:?} is not supported yet; the checkout is \
                 always {default}",
                self.resolved.file.display()
            ))
            .with_hint("remove sandbox.remote_path"));
        }
        Ok(())
    }

    /// The shell line `cheers <name> [args...]` runs: the `[commands]`
    /// entry with the arguments appended, else the script runner
    /// (`[run] script`) with the name and arguments.
    pub fn named_command(&self, name: &str, args: &[String]) -> Result<String, AppError> {
        let config = self.config();
        let quoted = || {
            args.iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let join = |head: &str| {
            let tail = quoted();
            if tail.is_empty() {
                head.to_string()
            } else {
                format!("{head} {tail}")
            }
        };
        if let Some(line) = config.commands.get(name) {
            return Ok(join(line.trim_end()));
        }
        if let Some(script) = &config.run.script {
            return Ok(join(&format!("{} {}", script.trim(), shell_quote(name))));
        }
        let known: Vec<&str> = config.commands.keys().map(String::as_str).collect();
        let known = if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        };
        Err(AppError::new(
            "unknown_command",
            format!(
                "no command named {name:?}: [commands] has {known} and no script runner \
                 ([run] script) is set"
            ),
        )
        .with_hint(format!(
            "add `{name} = \"...\"` under [commands] in {}, or run a command line with \
             `boatparty cheers -- <command>`",
            self.resolved.file.display()
        )))
    }
}

/// Project names must match `^[a-z0-9][a-z0-9-]{0,40}$`, so every name
/// derived from them is valid at the provider.
pub fn validate_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let valid = matches!(bytes.next(), Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.len() <= NAME_MAX
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "project name {name:?} is invalid: use 1 to {NAME_MAX} lowercase letters, digits \
             and hyphens, starting with a letter or digit"
        ))
    }
}

/// A valid project name derived from a directory name, if one can be.
pub fn default_name(dir_name: &str) -> Option<String> {
    let mut name = String::new();
    for c in dir_name.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if !(c == '-' && (name.is_empty() || name.ends_with('-'))) {
            name.push(c);
        }
    }
    name.truncate(NAME_MAX);
    let name = name.trim_end_matches('-').to_string();
    validate_name(&name).ok().map(|()| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_pattern() {
        for good in ["a", "app", "gg-desktop", "0x", &"a".repeat(41)] {
            assert!(validate_name(good).is_ok(), "{good}");
        }
        for bad in ["", "-a", "App", "a_b", "a.b", "a b", &"a".repeat(42), "é"] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_names_come_from_the_directory() {
        assert_eq!(default_name("gg-desktop").as_deref(), Some("gg-desktop"));
        assert_eq!(default_name("My_App.v2").as_deref(), Some("my-app-v2"));
        assert_eq!(default_name("--x--").as_deref(), Some("x"));
        assert_eq!(default_name("___").as_deref(), None);
        assert_eq!(default_name(&"b".repeat(60)).map(|n| n.len()), Some(41));
    }

    #[test]
    fn the_written_file_parses() {
        let text = config::detect::render_project_file("app", &Default::default());
        let config = ProjectConfig::parse(&text, Path::new(PROJECT_FILE)).unwrap();
        assert_eq!(config.name(), "app");
        assert!(
            ProjectConfig::parse(
                "version = 1\n[project]\nname = \"App\"\n",
                Path::new(PROJECT_FILE)
            )
            .is_err()
        );
        assert!(
            ProjectConfig::parse(
                "version = 1\n[project]\nname = \"a\"\nextra = 1\n",
                Path::new(PROJECT_FILE)
            )
            .is_err()
        );
        assert_eq!(config.history(), History::Head);
    }

    #[test]
    fn sync_history_is_read_and_unknown_keys_are_errors() {
        let path = Path::new(PROJECT_FILE);
        let full = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"full\"\n\
             [commands]\ntest = \"cargo test\"\n",
            path,
        )
        .unwrap();
        assert_eq!(full.history(), History::Full);
        let error = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"full\"\nlater = 1\n",
            path,
        )
        .unwrap_err();
        assert!(
            error.message().contains("unknown key sync.later"),
            "{error}"
        );
        let head = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"head\"\n",
            path,
        )
        .unwrap();
        assert_eq!(head.history(), History::Head);
        assert_eq!(head.requested_machine(), None);
        let small = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sandbox]\nmachine = \"small\"\n",
            path,
        )
        .unwrap();
        assert_eq!(small.requested_machine(), Some(MachineSize::Small));
        assert!(
            ProjectConfig::parse(
                "version = 1\n[project]\nname = \"a\"\n[sandbox]\nmachine = \"huge\"\n",
                path
            )
            .is_err()
        );
        assert!(head.seed_config().enabled);
        let off = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nseed = false\ngithub_token = false\n",
            path,
        )
        .unwrap();
        let seed = off.seed_config();
        assert!(!seed.enabled);
        assert_eq!(seed.token, crate::sync::seed::TokenSource::None);
        assert!(
            ProjectConfig::parse(
                "version = 1\n[project]\nname = \"a\"\n[sync]\nseed = \"no\"\n",
                path
            )
            .is_err()
        );
        let error = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"deep\"\n",
            path,
        )
        .unwrap_err();
        assert!(error.message().contains("history"), "{error}");
    }

    #[test]
    fn a_phase_3_minimal_file_and_this_repositorys_file_load_as_before() {
        let path = Path::new(PROJECT_FILE);
        let minimal = ProjectConfig::parse(
            "# boatparty project configuration. Check this file in.\n\
             version = 1\n\n[project]\nname = \"app\"\n",
            path,
        )
        .unwrap();
        assert_eq!(minimal.name(), "app");
        assert_eq!(minimal.history(), History::Head);
        assert_eq!(minimal.requested_machine(), None);
        assert_eq!(minimal.environment(), None);
        assert!(minimal.copy_back());
        assert!(minimal.seed_config().enabled);
        assert!(minimal.require_supported().is_ok());
        assert!(minimal.config().setup.is_empty() && minimal.config().ensure.is_empty());
        let own = ProjectConfig::parse(include_str!("../boatparty.toml"), path).unwrap();
        assert_eq!(own.name(), "boatparty");
    }

    #[test]
    fn named_commands_prefer_the_commands_table_then_the_script_runner() {
        let path = Path::new(PROJECT_FILE);
        let both = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\npresets = [\"bun\"]\n\
             [commands]\ntest = \"cargo test --locked\"\n",
            path,
        )
        .unwrap();
        assert_eq!(
            both.named_command("test", &["it's".into(), "-q".into()])
                .unwrap(),
            "cargo test --locked 'it'\\''s' -q"
        );
        assert_eq!(
            both.named_command("check:rust", &[]).unwrap(),
            "bun run check:rust"
        );
        let plain = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[commands]\nlint = \"x\"\ntest = \"y\"\n",
            path,
        )
        .unwrap();
        let error = plain.named_command("build", &[]).unwrap_err();
        assert_eq!(error.code(), "unknown_command");
        assert!(error.message().contains("lint, test"), "{error}");
        let custom = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sandbox]\nremote_path = \"/home/user/x\"\n",
            path,
        )
        .unwrap();
        assert!(custom.require_supported().is_err());
    }
}
