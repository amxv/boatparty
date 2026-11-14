//! The workspace (one checkout, identified by its Git toplevel) and its
//! project file, `boatparty.toml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::error::AppError;

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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub version: u32,
    pub project: ProjectSection,
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
        Ok(config)
    }

    pub fn name(&self) -> &str {
        &self.project.name
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
    }
}
