//! The workspace (one checkout, identified by its Git toplevel) and its
//! project file, `boatparty.toml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::AppError;
use crate::sync::History;

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

    /// Read this workspace's `boatparty.toml`.
    pub fn load_project(&self) -> Result<ProjectConfig, AppError> {
        let path = self.project_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AppError::config(format!(
                    "{} does not exist; run `boatparty init` in {} first",
                    path.display(),
                    self.root.display()
                )));
            }
            Err(error) => {
                return Err(AppError::io(
                    format!("cannot read {}", path.display()),
                    error,
                ));
            }
        };
        ProjectConfig::parse(&text, &path)
    }
}

/// The parts of `boatparty.toml` read so far. Sections and keys this
/// version does not know are ignored, so a file written for a later version
/// still loads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectConfig {
    pub version: u32,
    pub project: ProjectSection,
    #[serde(default)]
    pub sync: SyncSection,
}

/// `[sync]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SyncSection {
    /// `"head"` (the default) or `"full"`.
    pub history: Option<String>,
    /// Seed a member's first sync from the project's GitHub remote (default
    /// true).
    pub seed: Option<bool>,
    /// Pass a GitHub token (`BOATPARTY_GITHUB_TOKEN` or `gh auth token`) for
    /// a private remote's seed fetch (default true).
    pub github_token: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,
}

impl ProjectConfig {
    pub fn parse(text: &str, path: &Path) -> Result<Self, AppError> {
        let config: Self = toml::from_str(text)
            .map_err(|error| AppError::config(format!("{}: {error}", path.display())))?;
        if config.version != PROJECT_FILE_VERSION {
            return Err(AppError::config(format!(
                "{}: version {} is not supported (expected {PROJECT_FILE_VERSION})",
                path.display(),
                config.version
            )));
        }
        validate_name(&config.project.name)
            .map_err(|message| AppError::config(format!("{}: {message}", path.display())))?;
        if let Some(history) = &config.sync.history {
            History::parse(history).map_err(|message| {
                AppError::config(format!("{}: [sync] history: {message}", path.display()))
            })?;
        }
        Ok(config)
    }

    pub fn name(&self) -> &str {
        &self.project.name
    }

    /// How seeding from the GitHub remote is set up for this project.
    pub fn seed_config(&self) -> crate::sync::SeedConfig {
        crate::sync::SeedConfig::from_env(
            self.sync.seed.unwrap_or(true),
            self.sync.github_token.unwrap_or(true),
        )
    }

    /// How much Git history a sync sends.
    pub fn history(&self) -> History {
        self.sync
            .history
            .as_deref()
            .and_then(|value| History::parse(value).ok())
            .unwrap_or_default()
    }
}

/// The text `init` writes.
pub fn render_project_file(name: &str) -> String {
    format!(
        "# boatparty project configuration. Check this file in.\n\
         # The project name names this project's sandbox pool and its limits in\n\
         # the per-user account.toml, so keep it stable.\n\
         version = {PROJECT_FILE_VERSION}\n\
         \n\
         [project]\n\
         name = \"{name}\"\n"
    )
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
        let config =
            ProjectConfig::parse(&render_project_file("app"), Path::new(PROJECT_FILE)).unwrap();
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
    fn sync_history_is_read_and_unknown_sections_are_ignored() {
        let path = Path::new(PROJECT_FILE);
        let full = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"full\"\nlater = 1\n\
             [commands]\ntest = \"cargo test\"\n",
            path,
        )
        .unwrap();
        assert_eq!(full.history(), History::Full);
        let head = ProjectConfig::parse(
            "version = 1\n[project]\nname = \"a\"\n[sync]\nhistory = \"head\"\n",
            path,
        )
        .unwrap();
        assert_eq!(head.history(), History::Head);
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
}
