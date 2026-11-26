//! Manage the project's environment and its members' pins.
use super::WorkspaceContext;
use crate::{
    Context,
    cli::{EnvAction, SecretAction},
    error::AppError,
    provider::{Environment, EnvironmentPin, Sandbox},
    state::Store,
};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

fn version(store: &Store, project: &str, name: &str, version: u64) -> Result<(), AppError> {
    store.update(|state| {
        state
            .project_mut(project)
            .environment_versions
            .insert(name.to_string(), version);
        Ok(())
    })
}

fn remember_names(store: &Store, project: &str, env: &Environment) -> Result<(), AppError> {
    let names = if env.injects_secrets {
        env.names.clone()
    } else {
        Some(Vec::new())
    };
    if let Some(names) = names {
        store.update(|state| {
            let versions = state
                .project_mut(project)
                .environment_names
                .entry(env.id.clone())
                .or_default();
            versions.insert(env.version, names);
            while versions.len() > 32 {
                versions.pop_first();
            }
            Ok(())
        })?;
    }
    Ok(())
}

pub(super) fn expected_version(
    wctx: &WorkspaceContext,
    ctx: &Context,
) -> Result<Option<u64>, AppError> {
    let Some(name) = wctx.project.environment() else {
        return Ok(None);
    };
    let env = lookup(wctx.provider.environments()?, name)?;
    remember_names(&Store::new(&ctx.paths), wctx.project.name(), &env)?;
    version(
        &Store::new(&ctx.paths),
        wctx.project.name(),
        name,
        env.version,
    )?;
    Ok(Some(env.version))
}

fn lookup(environments: Vec<Environment>, name: &str) -> Result<Environment, AppError> {
    environments
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| AppError::config(format!("environment {name} does not exist")))
}

fn attach(wctx: &WorkspaceContext, name: &str) -> Result<(), AppError> {
    let path = wctx.workspace.project_file();
    let text =
        std::fs::read_to_string(&path).map_err(|e| AppError::io("reading project settings", e))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|_| AppError::config("cannot parse boatparty.toml"))?;
    if doc
        .get("sandbox")
        .and_then(|s| s.get("environment"))
        .is_some()
    {
        return Err(AppError::config(
            "project environment changed during creation; check boatparty.toml",
        ));
    }
    doc["sandbox"]["environment"] = toml_edit::value(name);
    crate::account::edit::write_atomically(&path, &doc.to_string())
}

pub fn secret(ctx: &Context, action: &SecretAction, create: bool) -> Result<u8, AppError> {
    let key = match action {
        SecretAction::Set { key } | SecretAction::Rm { key } => Some(key),
        SecretAction::List => None,
    };
    if key.is_some_and(|key| !crate::provider::valid_variable(key)) {
        return Err(AppError::new(
            "usage",
            "expected a variable name containing letters, digits or underscores, starting with a letter or underscore",
        ));
    }
    let value = if matches!(action, SecretAction::Set { .. }) {
        if std::io::stdin().is_terminal() {
            return Err(AppError::new(
                "usage",
                "redirect the value from a file or pipe on standard input",
            ));
        }
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|_| AppError::new("usage", "cannot read a UTF-8 value from standard input"))?;
        Some(text)
    } else {
        None
    };
    let wctx = WorkspaceContext::open(ctx)?;
    let project = wctx.project.name();
    let env = match wctx.project.environment() {
        Some(name) => lookup(wctx.provider.environments()?, name)?,
        None if create => {
            let env = wctx.provider.create_environment(project)?;
            attach(&wctx, project)?;
            env
        }
        None => {
            return Err(AppError::config(format!(
                "no project environment configured; add --create to create a secrets-only environment named {project}, or set [sandbox] environment"
            )));
        }
    };
    remember_names(&Store::new(&ctx.paths), project, &env)?;
    let (latest, result) = match action {
        SecretAction::List => {
            let names = env.names.ok_or_else(|| {
                AppError::new(
                    "invalid_response",
                    "variable names unavailable from provider",
                )
            })?;
            if !ctx.out.is_json() {
                for key in &names {
                    ctx.out.text(key);
                }
            }
            (env.version, json!({"names":names}))
        }
        _ => {
            let key = key.expect("set or rm");
            let latest = wctx
                .provider
                .change_secret(&env.id, key, value.as_deref())?;
            if !ctx.out.is_json() {
                ctx.out
                    .text(format_args!("{} v{latest}: updated {key}", env.name));
            }
            (latest, json!({"key":key}))
        }
    };
    version(&Store::new(&ctx.paths), project, &env.name, latest)?;
    if ctx.out.is_json() {
        let mut result = result;
        result["ok"] = json!(true);
        result["environment"] = json!(env.name);
        result["version"] = json!(latest);
        ctx.out.document(&result);
    }
    Ok(0)
}

fn variable_names(
    pin: &EnvironmentPin,
    envs: &[Environment],
    cached: Option<&crate::state::ProjectState>,
) -> Option<Vec<String>> {
    let Some(name) = &pin.name else {
        return Some(Vec::new());
    };
    let env = envs.iter().find(|e| e.name == *name)?;
    if Some(env.version) != pin.version {
        return cached?
            .environment_names
            .get(&env.id)?
            .get(&pin.version?)
            .cloned();
    }
    if env.injects_secrets {
        env.names.clone()
    } else {
        Some(Vec::new())
    }
}

pub fn run(ctx: &Context, action: &EnvAction) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let expected = wctx.project.environment();
    let envs = wctx.provider.environments()?;
    let target = expected
        .map(|name| {
            envs.iter()
                .find(|e| e.name == name)
                .ok_or_else(|| AppError::config(format!("environment {name} does not exist")))
        })
        .transpose()?;
    let latest = target.map(|env| env.version);
    if let Some(e) = target {
        remember_names(&pool.store, &pool.project, e)?;
        version(&pool.store, &pool.project, &e.name, e.version)?;
    }
    let selected = match action {
        EnvAction::Apply { member, all: false } => Some(
            pool.resolve(member.as_deref())?
                .id
                .ok_or_else(|| AppError::new("no_member", "member has no sandbox yet"))?,
        ),
        _ => None,
    };
    let cache = pool.store.load()?;
    let mut rows = Vec::new();
    for mut sandbox in wctx
        .provider
        .list_by_label(crate::pool::LABEL_PROJECT, &pool.project)?
    {
        if selected.as_ref().is_some_and(|id| *id != sandbox.id) {
            continue;
        }
        let mut result = "current";
        let member = sandbox.name.clone().unwrap_or_else(|| sandbox.id.clone());
        if matches!(action, EnvAction::Apply { .. })
            && sandbox.environment.warning(expected, latest).is_some()
        {
            if let Some(env) =
                target.filter(|e| Some(e.name.as_str()) == sandbox.environment.name.as_deref())
            {
                wctx.provider.upgrade_environment(&env.id, &sandbox.id)?;
                sandbox = wctx.provider.get(&sandbox.id)?;
                result = "upgraded";
            }
            if sandbox.environment.warning(expected, latest).is_some() {
                result = "recreate";
                ctx.out.note(format_args!("{member} needs recreation for a member-only change: boatparty rm {member}, then rerun the command that uses it"));
                if let Some(env) =
                    target.filter(|e| Some(e.name.as_str()) == sandbox.environment.name.as_deref())
                {
                    ctx.out.note(format_args!("to upgrade every sandbox on this environment instead, run boat env upgrade {}", env.name));
                }
            }
        }
        let names = variable_names(&sandbox.environment, &envs, cache.project(&pool.project));
        let warning = sandbox.environment.warning(expected, latest);
        if !ctx.out.is_json() {
            ctx.out.text(format_args!(
                "{member}: {} v{}; variables: {}{}",
                sandbox.environment.name.as_deref().unwrap_or("none"),
                sandbox
                    .environment
                    .version
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
                names
                    .as_ref()
                    .map(|n| n.join(", "))
                    .unwrap_or_else(|| "unavailable for this pin".into()),
                warning
                    .as_ref()
                    .map(|w| format!("; {w}"))
                    .unwrap_or_default()
            ));
        }
        remember(&pool.store, &pool.project, &sandbox, expected)?;
        rows.push(json!({"member":member,"id":sandbox.id,"environment":sandbox.environment.name,"version":sandbox.environment.version,
            "names":names,"warning":warning,"action":if matches!(action,EnvAction::Status){Value::Null}else{json!(result)}}));
    }
    if selected.is_some() && rows.is_empty() {
        return Err(AppError::new(
            "no_member",
            "selected sandbox is no longer in the project listing",
        ));
    }
    if ctx.out.is_json() {
        ctx.out
            .document(&json!({"ok":true,"environment":expected,"version":latest,"members":rows}));
    }
    Ok(0)
}

pub(super) fn remember(
    store: &Store,
    project: &str,
    sandbox: &Sandbox,
    expected: Option<&str>,
) -> Result<(), AppError> {
    store.update(|state| {
        if let Some(member) = state
            .project_mut(project)
            .members
            .iter_mut()
            .find(|m| m.id.as_deref() == Some(&sandbox.id))
        {
            member.environment = Some(sandbox.environment.clone());
            member.expected_environment = expected.map(str::to_owned);
        }
        Ok(())
    })
}
