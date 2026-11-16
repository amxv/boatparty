//! Value checks on the effective configuration. Each error names the value's
//! path and the layer it came from.

use std::collections::BTreeMap;

use super::{Config, Origin};
use crate::provider::MachineSize;
use crate::sync::History;

pub fn check(config: &Config, origins: &BTreeMap<String, Origin>) -> Result<(), String> {
    let at = |path: &str| match origins.get(path) {
        Some(origin) => format!("{path} (from {origin})"),
        None => path.to_string(),
    };

    if let Some(machine) = &config.sandbox.machine {
        machine
            .parse::<MachineSize>()
            .map_err(|message| format!("{}: {message}", at("sandbox.machine")))?;
    }
    remote_path(&config.sandbox.remote_path)
        .map_err(|message| format!("{}: {message}", at("sandbox.remote_path")))?;
    if let Some(environment) = &config.sandbox.environment {
        environment_name(environment)
            .map_err(|message| format!("{}: {message}", at("sandbox.environment")))?;
    }
    History::parse(&config.sync.history)
        .map_err(|message| format!("{}: {message}", at("sync.history")))?;

    for (key, value) in &config.run.env {
        let path = format!("run.env.{key}");
        env_name(key).map_err(|message| format!("{}: {message}", at(&path)))?;
        no_control(value, false).map_err(|message| format!("{}: {message}", at(&path)))?;
    }
    if let Some(script) = &config.run.script {
        non_empty(script).map_err(|message| format!("{}: {message}", at("run.script")))?;
    }

    for step in &config.setup {
        let base = format!("setup.{}", step.name);
        step_name(&step.name)
            .map_err(|message| format!("{}: {message}", at(&format!("{base}.name"))))?;
        non_empty(&step.run)
            .map_err(|message| format!("{}: {message}", at(&format!("{base}.run"))))?;
    }
    for step in &config.ensure {
        let base = format!("ensure.{}", step.name);
        let here = |key: &str| at(&format!("{base}.{key}"));
        step_name(&step.name).map_err(|message| format!("{}: {message}", here("name")))?;
        non_empty(&step.run).map_err(|message| format!("{}: {message}", here("run")))?;
        if let Some(dir) = &step.dir {
            relative_path(dir, false).map_err(|message| format!("{}: {message}", here("dir")))?;
        }
        for file in &step.key_files {
            relative_path(file, false)
                .map_err(|message| format!("{}: {message}", here("key_files")))?;
        }
    }

    for dir in &config.build.output_dirs {
        relative_path(dir, true)
            .map_err(|message| format!("{}: {message}", at("build.output_dirs")))?;
    }
    for name in &config.build.cache_dirs {
        cache_name(name).map_err(|message| format!("{}: {message}", at("build.cache_dirs")))?;
    }
    for file in &config.base.stale_inputs {
        relative_path(file, false)
            .map_err(|message| format!("{}: {message}", at("base.stale_inputs")))?;
    }
    if let Some(git_ref) = &config.base.git_ref
        && (git_ref.is_empty() || git_ref.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(format!("{}: {git_ref:?} is not a Git ref", at("base.ref")));
    }
    if let Some(warm) = &config.base.warm {
        non_empty(warm).map_err(|message| format!("{}: {message}", at("base.warm")))?;
    }

    for (name, line) in &config.commands {
        let path = format!("commands.{name}");
        command_name(name).map_err(|message| format!("{}: {message}", at(&path)))?;
        non_empty(line).map_err(|message| format!("{}: {message}", at(&path)))?;
    }
    Ok(())
}

/// A checkout path on the sandbox: absolute, under /home/user (the only
/// durable home, and where signed transfers may reach), not the tool's own
/// state directory and not ~/.cache (lost on a new machine).
fn remote_path(path: &str) -> Result<(), String> {
    let rest = path
        .strip_prefix("/home/user/")
        .ok_or_else(|| format!("{path:?} must be an absolute path under /home/user/"))?;
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err(format!("{path:?} must name a directory below /home/user/"));
    }
    relative_path(rest, false)?;
    if !rest
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "{path:?} may only use letters, digits, '/', '.', '_' and '-'"
        ));
    }
    let first = rest.split('/').next().unwrap_or_default();
    if matches!(first, ".boatparty" | ".cache") {
        return Err(format!(
            "{path:?} is inside ~/{first}, which boatparty reserves or the sandbox does not keep"
        ));
    }
    Ok(())
}

/// A directory name matched at any depth, globs allowed.
fn cache_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(format!(
            "{name:?} must be a directory name (no '/'), such as \"incremental\""
        ));
    }
    no_control(name, false)
}

fn environment_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not an environment name (letters, digits, '-', '_' and '.')"
        ))
    }
}

fn env_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let ok = matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !ok {
        return Err(format!("{name:?} is not a valid environment variable name"));
    }
    if name.starts_with("BOATPARTY") {
        return Err(format!(
            "{name:?}: names starting with BOATPARTY are reserved"
        ));
    }
    Ok(())
}

/// Step names are part of origin paths and of the sandbox's ensure-key
/// file names.
fn step_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let ok = matches!(bytes.next(), Some(b) if b.is_ascii_alphanumeric())
        && name.len() <= 64
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "step name {name:?} is invalid: use letters, digits, '-' and '_'"
        ))
    }
}

/// Command names are what follows `boatparty run`, so `check:rust` works.
pub fn command_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let ok = matches!(bytes.next(), Some(b) if b.is_ascii_alphanumeric() || b == b'_')
        && bytes
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.' | b'+'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "command name {name:?} is invalid: start with a letter, digit or '_', then use \
             letters, digits and - _ : . +"
        ))
    }
}

/// A path relative to the checkout that stays inside it. `*` and `?`
/// globs are allowed where `globs` is set.
fn relative_path(path: &str, globs: bool) -> Result<(), String> {
    if path.is_empty() {
        return Err("a path must not be empty".to_string());
    }
    if path.starts_with('/') || path.starts_with('~') {
        return Err(format!("{path:?} must be relative to the checkout"));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(format!("{path:?} must stay inside the checkout (no '..')"));
    }
    if !globs && path.contains(['*', '?', '[']) {
        return Err(format!("{path:?}: globs are not allowed here"));
    }
    no_control(path, false)
}

fn non_empty(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        Err("must not be empty".to_string())
    } else {
        no_control(text, true)
    }
}

fn no_control(text: &str, newlines: bool) -> Result<(), String> {
    if text
        .chars()
        .any(|c| c == '\0' || (c.is_control() && !(newlines && (c == '\n' || c == '\t'))))
    {
        Err("must not contain control characters".to_string())
    } else {
        Ok(())
    }
}
