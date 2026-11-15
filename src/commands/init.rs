//! `boatparty init`: write `boatparty.toml` and register the project.

use std::fs::OpenOptions;
use std::io::Write;

use serde_json::json;

use super::load_account;
use crate::Context;
use crate::account;
use crate::cli::InitArgs;
use crate::config::detect;
use crate::error::AppError;
use crate::project::{self, Workspace};

pub fn run(ctx: &Context, args: &InitArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let path = workspace.project_file();
    let mut detected = None;
    let (name, wrote) = if path.exists() {
        let existing = workspace.load_project(&ctx.paths)?;
        if let Some(asked) = &args.name
            && asked != existing.name()
        {
            return Err(AppError::config(format!(
                "{} already names the project {:?}; edit it to rename the project",
                path.display(),
                existing.name()
            )));
        }
        (existing.name().to_string(), false)
    } else {
        let name = match &args.name {
            Some(name) => {
                project::validate_name(name).map_err(AppError::config)?;
                name.clone()
            }
            None => default_name(&workspace)?,
        };
        // Stacks are detected here, once; nothing is detected at run time.
        let found = detect::detect(&workspace.root);
        write_new(&path, &detect::render_project_file(&name, &found))?;
        detected = Some(found);
        (name, true)
    };
    load_account(ctx)?;
    let registered = account::config::register_project(&ctx.paths, &name)?;

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": name,
            "workspace": workspace.key(),
            "file": path.display().to_string(),
            "file_created": wrote,
            "presets": detected.as_ref().map(|found| found.presets.clone()),
            "commands": detected.as_ref().map(|found| {
                found
                    .commands
                    .iter()
                    .map(|(name, line)| (name.clone(), json!(line)))
                    .collect::<serde_json::Map<_, _>>()
            }),
            "detected_from": detected.as_ref().map(|found| found.markers.clone()),
            "registered": registered,
            "account_file": ctx.paths.account_file().display().to_string(),
        }));
    } else {
        if wrote {
            ctx.out
                .text(format_args!("wrote {} for project {name}", path.display()));
            if let Some(found) = &detected {
                if found.presets.is_empty() {
                    ctx.out.text(
                        "no stack detected; add presets to [project] presets (`boatparty preset list`)",
                    );
                } else {
                    ctx.out.text(format_args!(
                        "presets: {} (from {})",
                        found.presets.join(", "),
                        found.markers.join(", ")
                    ));
                }
                for (name, line) in &found.commands {
                    ctx.out.text(format_args!(
                        "command: `boatparty cheers {name}` runs `{line}`"
                    ));
                }
            }
        } else {
            ctx.out.text(format_args!(
                "{} already exists (project {name})",
                path.display()
            ));
        }
        if registered {
            ctx.out.text(format_args!(
                "registered project {name} in {}",
                ctx.paths.account_file().display()
            ));
        }
        ctx.out
            .text("next: `boatparty launch` starts this workspace's sandbox");
    }
    Ok(0)
}

fn default_name(workspace: &Workspace) -> Result<String, AppError> {
    let dir = workspace
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    project::default_name(&dir).ok_or_else(|| {
        AppError::config(format!(
            "cannot derive a project name from the directory name {dir:?}"
        ))
        .with_hint("pass one with `boatparty init --name <name>`")
    })
}

fn write_new(path: &std::path::Path, contents: &str) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| AppError::io(format!("cannot create {}", path.display()), error))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))
}
