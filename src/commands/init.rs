//! `boatparty init`: write `boatparty.toml` and register the project.

use std::fs::OpenOptions;
use std::io::Write;

use serde_json::json;

use super::load_account;
use crate::Context;
use crate::account;
use crate::cli::InitArgs;
use crate::error::AppError;
use crate::project::{self, Workspace};

pub fn run(ctx: &Context, args: &InitArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let path = workspace.project_file();
    let (name, wrote) = if path.exists() {
        let existing = workspace.load_project()?;
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
        write_new(&path, &project::render_project_file(&name))?;
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
            "registered": registered,
            "account_file": ctx.paths.account_file().display().to_string(),
        }));
    } else {
        if wrote {
            ctx.out
                .text(format_args!("wrote {} for project {name}", path.display()));
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
