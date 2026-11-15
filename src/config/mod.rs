//! Project configuration: `boatparty.toml` composed from presets.
//!
//! Layers, applied in order: boatparty's defaults, then each preset named in
//! `[project] presets` (in the listed order), then the project file itself,
//! which wins. Tables merge by key and every other value is replaced whole
//! (a list from a later layer replaces the earlier list), except that a
//! `[run.env]` value naming its own variable (`PATH = "/x/bin:$PATH"`)
//! extends the earlier value. `[[setup]]` and
//! `[[ensure]]` steps merge by `name`: a later step with a known name
//! replaces only the keys it sets, a new name is appended, and
//! `enabled = false` switches a step off.
//!
//! Step meaning, fixed here for the runner (Phase 10) and the base build
//! (Phase 11):
//! - a `setup` step runs after every create or resume of a sandbox (before
//!   the first command of each boot of its machine; a resume that reclaims
//!   the kept machine keeps its effects), unless its `check` succeeds;
//! - an `ensure` step runs before a command when its key (the content of
//!   `key_files` plus the output of `key_command`) changed since it last
//!   succeeded, or when its `check` fails.
//!
//! Nothing here is detected at run time; `boatparty init` detects stacks
//! once and writes an explicit presets list.

pub mod detect;
mod merge;
pub mod presets;
pub mod render;
pub mod schema;
mod validate;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::{Table, Value};

use crate::error::AppError;
use crate::paths::Paths;
use crate::project::PROJECT_FILE;
use presets::{PresetDirs, PresetFile, Source};
use schema::FileKind;

/// The schema reference printed by `boatparty config --help`.
pub const HELP: &str = "boatparty.toml (version = 1). Every key is optional except version and \
[project] name; an unknown key is an error.

  [project]   name, presets = [\"rust\", ...] (applied in order; this file wins)
  [sandbox]   machine (small|default|large|xlarge), remote_path (the checkout; default \
/home/user/<name>, any directory under /home/user except ~/.boatparty and ~/.cache), environment (a boat environment for new sandboxes; unset = no account secrets)
  [sync]      history (\"head\"|\"full\"), copy_back, seed, github_token (true|false)
  [run]       ram_tmp (true|false), script (the script runner for `cheers <name>`, e.g. \"bun run\"), \
git_identity (true|false: commands commit as the local user.name and user.email), github_token \
(true|false: git in commands authenticates to github.com over HTTPS with BOATPARTY_GITHUB_TOKEN, BOATPARTY_GITHUB_TOKEN_COMMAND or \
`gh auth token`, served for the command's lifetime only, never stored on the sandbox; a project \
whose [sandbox] environment has GitHub access can use that instead, but boat then stores the login \
on the sandbox)
  [run.env]   NAME = \"value\": exported before steps and commands; values are expanded by bash \
in the checkout ($HOME, $PATH, $PWD); a value that names its own variable (\"/x/bin:$PATH\" for PATH) \
extends the value from earlier layers instead of replacing it
  [[setup]]   name, check, run: runs after every create or resume unless check succeeds
  [[ensure]]  name, dir, key_files, key_command, check, run: runs before a command when its key \
(the content of key_files, relative to dir, plus key_command's output) changed since it last \
succeeded, or when check fails
  [build]     output_dirs (relative, globs allowed), floor_mtimes (true|false), cache_dirs \
(directory names under output_dirs holding a compiler's own caches: not floored, dropped when a \
restore left a file in them unreadable)
  [base]      warm, stale_inputs, ref (used by `base build`)
  [commands]  NAME = \"shell line\": `boatparty cheers NAME [args...]`

A preset has the same shape without [project]. Layers apply in order: defaults, each preset, \
boatparty.toml. Tables merge by key; other values (lists too) are replaced. Steps merge by name: \
a later step with a known name replaces only the keys it sets, a new name is appended, and \
`enabled = false` switches a step off. The runner applies [run], the steps and [build] on the \
sandbox; [base] is stored for `base build`.";

/// Where a value of the effective configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// boatparty's own default.
    Default,
    Preset {
        name: String,
        source: Source,
    },
    /// The project's `boatparty.toml`.
    File,
    /// A `[run.env]` value that extends its own earlier value (`$PATH` in
    /// `PATH`): the latest layer first.
    Several(Vec<Origin>),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::Preset { name, source } => write!(f, "preset {name} ({source})"),
            Self::File => f.write_str(PROJECT_FILE),
            Self::Several(layers) => {
                let names: Vec<String> = layers.iter().map(ToString::to_string).collect();
                write!(f, "{}", names.join(", extending "))
            }
        }
    }
}

/// The effective configuration of one project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub project: Project,
    pub sandbox: Sandbox,
    pub sync: Sync,
    pub run: Run,
    #[serde(default)]
    pub setup: Vec<SetupStep>,
    #[serde(default)]
    pub ensure: Vec<EnsureStep>,
    pub build: Build,
    #[serde(default)]
    pub base: Base,
    #[serde(default)]
    pub commands: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub presets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    /// small, default, large or xlarge; unset means account.toml's default.
    pub machine: Option<String>,
    /// The checkout on the sandbox.
    pub remote_path: String,
    /// A boat environment to attach to new sandboxes. Unset: no account
    /// secrets at all (`noEnv`).
    pub environment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    pub history: String,
    pub copy_back: bool,
    pub seed: bool,
    pub github_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub ram_tmp: bool,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The script runner for `cheers <name>`, for example `"bun run"`.
    pub script: Option<String>,
    /// Commands commit as the local repository's `user.name` and
    /// `user.email`.
    pub git_identity: bool,
    /// Commands get a GitHub token for github.com over HTTPS (private
    /// dependencies), served for the command's lifetime only.
    pub github_token: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStep {
    pub name: String,
    pub check: Option<String>,
    pub run: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsureStep {
    pub name: String,
    /// Relative to the checkout; key files are relative to it.
    pub dir: Option<String>,
    #[serde(default)]
    pub key_files: Vec<String>,
    pub key_command: Option<String>,
    pub check: Option<String>,
    pub run: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub output_dirs: Vec<String>,
    pub floor_mtimes: bool,
    /// Names of directories under the output directories that hold a
    /// compiler's own content-addressed caches: never floored, and dropped
    /// when a restore left a file in them unreadable.
    #[serde(default)]
    pub cache_dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    pub warm: Option<String>,
    #[serde(default)]
    pub stale_inputs: Vec<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

/// A step a layer switched off with `enabled = false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchedOff {
    /// `setup` or `ensure`.
    pub section: String,
    pub name: String,
    pub by: Origin,
}

/// A project's configuration with everything needed to explain it.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The project file.
    pub file: PathBuf,
    /// The project file as written (parsed, nothing applied).
    pub own: Table,
    /// The effective configuration as a table, defaults included.
    pub merged: Table,
    /// Where each value of `merged` came from, by dotted path
    /// (`sync.history`, `run.env.CARGO_HOME`, `ensure.cargo-fetch.run`).
    pub origins: BTreeMap<String, Origin>,
    /// The presets applied, in order.
    pub presets: Vec<PresetFile>,
    pub switched_off: Vec<SwitchedOff>,
    pub config: Config,
}

/// The checkout on every member when `remote_path` is not set.
pub fn default_remote_path(project: &str) -> String {
    format!("/home/user/{project}")
}

/// Read and resolve the configuration of the workspace at `root`.
pub fn load(root: &Path, paths: &Paths) -> Result<Resolved, AppError> {
    let file = root.join(PROJECT_FILE);
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::config(format!(
                "{} does not exist; run `boatparty init` in {} first",
                file.display(),
                root.display()
            )));
        }
        Err(error) => {
            return Err(AppError::io(
                format!("cannot read {}", file.display()),
                error,
            ));
        }
    };
    let dirs = PresetDirs::new(paths.user_presets_dir(), Some(root));
    resolve(&text, &file, &dirs)
}

/// Resolve a project file's text against the presets in `dirs`.
pub fn resolve(text: &str, file: &Path, dirs: &PresetDirs) -> Result<Resolved, AppError> {
    let own = parse(text, FileKind::Project)
        .map_err(|message| AppError::config(format!("{}: {message}", file.display())))?;
    let project = own
        .get("project")
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    let name = project
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    crate::project::validate_name(&name)
        .map_err(|message| AppError::config(format!("{}: {message}", file.display())))?;
    let wanted: Vec<String> = project
        .get("presets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut merger = merge::Merger::default();
    merger.apply(&defaults(&name), &Origin::Default);
    let mut applied = Vec::new();
    for (index, preset) in wanted.iter().enumerate() {
        if wanted[..index].contains(preset) {
            return Err(AppError::config(format!(
                "{}: project.presets lists {preset:?} twice",
                file.display()
            )));
        }
        let found = load_preset(preset, dirs).map_err(|message| {
            AppError::config(format!("{}: project.presets: {message}", file.display()))
        })?;
        let table = parse(&found.text, FileKind::Preset).map_err(|message| {
            AppError::config(format!("preset {preset} ({}): {message}", found.source))
        })?;
        merger.apply(
            &table,
            &Origin::Preset {
                name: preset.clone(),
                source: found.source.clone(),
            },
        );
        applied.push(found);
    }
    merger.apply(&own, &Origin::File);
    let (merged, origins, switched_off) = merger
        .finish()
        .map_err(|message| AppError::config(format!("{}: {message}", file.display())))?;
    let config: Config = Value::Table(merged.clone())
        .try_into()
        .map_err(|error| AppError::config(format!("{}: {error}", file.display())))?;
    validate::check(&config, &origins)
        .map_err(|message| AppError::config(format!("{}: {message}", file.display())))?;
    Ok(Resolved {
        file: file.to_path_buf(),
        own,
        merged,
        origins,
        presets: applied,
        switched_off,
        config,
    })
}

/// Check one preset file on its own: its shape and its values.
pub fn check_preset(preset: &PresetFile) -> Result<(), AppError> {
    let fail = |message: String| {
        AppError::config(format!(
            "preset {} ({}): {message}",
            preset.name, preset.source
        ))
    };
    let table = parse(&preset.text, FileKind::Preset).map_err(fail)?;
    let mut merger = merge::Merger::default();
    merger.apply(&defaults("preset-check"), &Origin::Default);
    let origin = Origin::Preset {
        name: preset.name.clone(),
        source: preset.source.clone(),
    };
    merger.apply(&table, &origin);
    let mut project = Table::new();
    project.insert("name".into(), Value::String("preset-check".into()));
    let mut top = Table::new();
    top.insert("version".into(), Value::Integer(1));
    top.insert("project".into(), Value::Table(project));
    merger.apply(&top, &Origin::Default);
    let (merged, origins, _) = merger.finish().map_err(fail)?;
    let config: Config = Value::Table(merged)
        .try_into()
        .map_err(|error| fail(error.to_string()))?;
    validate::check(&config, &origins).map_err(fail)
}

fn load_preset(name: &str, dirs: &PresetDirs) -> Result<PresetFile, String> {
    presets::valid_name(name)?;
    match dirs.find(name) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => {
            let known = dirs.names().unwrap_or_default().join(", ");
            Err(format!(
                "no preset named {name:?} (available: {known}); `boatparty preset list` shows \
                 where each one lives"
            ))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn parse(text: &str, kind: FileKind) -> Result<Table, String> {
    let table: Table = toml::from_str(text).map_err(|error| error.to_string())?;
    schema::check(&table, kind)?;
    Ok(table)
}

/// boatparty's defaults, the bottom layer.
fn defaults(project: &str) -> Table {
    let text = format!(
        "[sandbox]\nremote_path = {remote}\n\
         [sync]\nhistory = \"head\"\ncopy_back = true\nseed = true\ngithub_token = true\n\
         [run]\nram_tmp = true\ngit_identity = true\ngithub_token = false\n\
         [build]\noutput_dirs = []\nfloor_mtimes = true\ncache_dirs = []\n",
        remote = Value::String(default_remote_path(project))
    );
    toml::from_str(&text).expect("the defaults parse")
}

#[cfg(test)]
mod tests;
