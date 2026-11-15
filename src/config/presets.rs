//! Presets: TOML files with the shape of `boatparty.toml` minus `[project]`.
//!
//! Three places hold them, from least to most specific: the built-ins
//! embedded in the binary, the user's presets directory
//! (`~/.config/boatparty/presets/`) and the project's `.boatparty/presets/`.
//! A preset file in a more specific place replaces one of the same name in
//! a less specific place entirely; that is what `preset eject` relies on.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;

/// The built-in presets, as the text files in `src/config/presets/`.
pub const BUILT_IN: &[(&str, &str)] = &[
    ("rust", include_str!("presets/rust.toml")),
    ("node", include_str!("presets/node.toml")),
    ("pnpm", include_str!("presets/pnpm.toml")),
    ("bun", include_str!("presets/bun.toml")),
    ("python", include_str!("presets/python.toml")),
    ("go", include_str!("presets/go.toml")),
];

/// The project's presets directory, relative to the workspace root.
pub const PROJECT_PRESETS_DIR: &str = ".boatparty/presets";

/// Where a preset file comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    User(PathBuf),
    Project(PathBuf),
}

impl Source {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::User(_) => "user",
            Self::Project(_) => "project",
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::BuiltIn => None,
            Self::User(path) | Self::Project(path) => Some(path),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn => f.write_str("built-in"),
            Self::User(path) | Self::Project(path) => {
                write!(f, "{}: {}", self.kind(), path.display())
            }
        }
    }
}

/// One preset file.
#[derive(Debug, Clone)]
pub struct PresetFile {
    pub name: String,
    pub source: Source,
    pub text: String,
}

impl PresetFile {
    /// The first comment line of the file, as a one-line summary.
    pub fn summary(&self) -> String {
        self.text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with('#'))
            .map(|line| line.trim_start_matches('#').trim().to_string())
            .unwrap_or_default()
    }
}

/// The two preset directories on disk.
#[derive(Debug, Clone)]
pub struct PresetDirs {
    pub user: PathBuf,
    /// `None` outside a workspace.
    pub project: Option<PathBuf>,
}

impl PresetDirs {
    pub fn new(user: PathBuf, workspace_root: Option<&Path>) -> Self {
        Self {
            user,
            project: workspace_root.map(|root| root.join(PROJECT_PRESETS_DIR)),
        }
    }

    /// The preset that `name` refers to: the project's, else the user's,
    /// else the built-in.
    pub fn find(&self, name: &str) -> Result<Option<PresetFile>, AppError> {
        Ok(self.candidates(name)?.into_iter().next())
    }

    /// Every file named `name`, most specific first.
    pub fn candidates(&self, name: &str) -> Result<Vec<PresetFile>, AppError> {
        let mut found = Vec::new();
        if valid_name(name).is_err() {
            return Ok(found);
        }
        if let Some(dir) = &self.project {
            let path = dir.join(format!("{name}.toml"));
            if let Some(text) = read_optional(&path)? {
                found.push(PresetFile {
                    name: name.to_string(),
                    source: Source::Project(path),
                    text,
                });
            }
        }
        let path = self.user.join(format!("{name}.toml"));
        if let Some(text) = read_optional(&path)? {
            found.push(PresetFile {
                name: name.to_string(),
                source: Source::User(path),
                text,
            });
        }
        if let Some(text) = built_in(name) {
            found.push(PresetFile {
                name: name.to_string(),
                source: Source::BuiltIn,
                text: text.to_string(),
            });
        }
        Ok(found)
    }

    /// Every preset name available, sorted, built-ins first.
    pub fn names(&self) -> Result<Vec<String>, AppError> {
        let mut names: Vec<String> = BUILT_IN.iter().map(|(name, _)| name.to_string()).collect();
        let mut extra = Vec::new();
        for dir in [Some(&self.user), self.project.as_ref()]
            .into_iter()
            .flatten()
        {
            for name in names_in(dir)? {
                if !names.contains(&name) && !extra.contains(&name) {
                    extra.push(name);
                }
            }
        }
        extra.sort();
        names.extend(extra);
        Ok(names)
    }

    /// Every preset file on disk (user and project), for validation.
    pub fn files_on_disk(&self) -> Result<Vec<PresetFile>, AppError> {
        let mut files = Vec::new();
        for dir in [self.project.as_ref(), Some(&self.user)]
            .into_iter()
            .flatten()
        {
            for name in names_in(dir)? {
                let path = dir.join(format!("{name}.toml"));
                if let Some(text) = read_optional(&path)? {
                    let source = if Some(dir) == self.project.as_ref() {
                        Source::Project(path)
                    } else {
                        Source::User(path)
                    };
                    files.push(PresetFile { name, source, text });
                }
            }
        }
        Ok(files)
    }
}

pub fn built_in(name: &str) -> Option<&'static str> {
    BUILT_IN
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, text)| *text)
}

/// Preset names use the project-name alphabet: lowercase letters, digits
/// and hyphens.
pub fn valid_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let ok = matches!(bytes.next(), Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.len() <= 41
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "preset name {name:?} is invalid: use lowercase letters, digits and hyphens"
        ))
    }
}

/// The preset names (`<name>.toml`) in a directory; a missing directory has none.
fn names_in(dir: &Path) -> Result<Vec<String>, AppError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::io(
                format!("cannot read {}", dir.display()),
                error,
            ));
        }
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if let Some(name) = file_name.strip_suffix(".toml")
            && valid_name(name).is_ok()
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

fn read_optional(path: &Path) -> Result<Option<String>, AppError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::io(
            format!("cannot read {}", path.display()),
            error,
        )),
    }
}
