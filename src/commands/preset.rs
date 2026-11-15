//! `boatparty preset list|show|eject`: presets as plain text files that an
//! agent or a person can read, copy and edit.

use std::fs;

use serde_json::json;

use crate::Context;
use crate::cli::{PresetAction, PresetArgs, PresetEjectArgs};
use crate::config::presets::{self, PROJECT_PRESETS_DIR, PresetDirs, Source};
use crate::error::AppError;
use crate::project::Workspace;

pub fn run(ctx: &Context, args: &PresetArgs) -> Result<u8, AppError> {
    match &args.action {
        PresetAction::List => list(ctx),
        PresetAction::Show { name } => show(ctx, name),
        PresetAction::Eject(eject_args) => eject(ctx, eject_args),
    }
}

/// The preset directories, with the project's when run inside a workspace.
fn dirs(ctx: &Context) -> (PresetDirs, Option<Workspace>) {
    let workspace = Workspace::current().ok();
    let dirs = PresetDirs::new(
        ctx.paths.user_presets_dir(),
        workspace.as_ref().map(|workspace| workspace.root.as_path()),
    );
    (dirs, workspace)
}

fn list(ctx: &Context) -> Result<u8, AppError> {
    let (dirs, workspace) = dirs(ctx);
    // The presets this project names, when its file can be read at all.
    let used: Option<Vec<String>> = workspace.as_ref().and_then(|workspace| {
        crate::config::load(&workspace.root, &ctx.paths)
            .ok()
            .map(|resolved| resolved.config.project.presets)
    });
    let mut rows = Vec::new();
    for name in dirs.names()? {
        let candidates = dirs.candidates(&name)?;
        let Some(effective) = candidates.first() else {
            continue;
        };
        let shadows: Vec<String> = candidates[1..]
            .iter()
            .map(|file| file.source.to_string())
            .collect();
        rows.push(json!({
            "name": name,
            "source": effective.source.kind(),
            "path": effective.source.path().map(|path| path.display().to_string()),
            "shadows": shadows,
            "used": used.as_ref().map(|used| used.contains(&name)),
            "summary": effective.summary(),
        }));
    }
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "user_dir": dirs.user.display().to_string(),
            "project_dir": dirs.project.as_ref().map(|dir| dir.display().to_string()),
            "presets": rows,
        }));
        return Ok(0);
    }
    let width = rows
        .iter()
        .filter_map(|row| row["name"].as_str())
        .map(str::len)
        .max()
        .unwrap_or(4)
        .max(4);
    ctx.out.text(format_args!(
        "{:width$}  {:8}  {:4}  SUMMARY",
        "NAME", "SOURCE", "USED"
    ));
    for row in &rows {
        let used = match row["used"].as_bool() {
            Some(true) => "yes",
            Some(false) => "no",
            None => "-",
        };
        let mut summary = row["summary"].as_str().unwrap_or_default().to_string();
        if let Some(path) = row["path"].as_str() {
            summary = format!("{summary} ({path})");
        }
        if let Some(shadows) = row["shadows"].as_array()
            && !shadows.is_empty()
        {
            let shadows: Vec<&str> = shadows.iter().filter_map(|s| s.as_str()).collect();
            summary = format!("{summary}; replaces {}", shadows.join(", "));
        }
        ctx.out.text(format_args!(
            "{:width$}  {:8}  {:4}  {summary}",
            row["name"].as_str().unwrap_or_default(),
            row["source"].as_str().unwrap_or_default(),
            used,
        ));
    }
    Ok(0)
}

fn show(ctx: &Context, name: &str) -> Result<u8, AppError> {
    let (dirs, _) = dirs(ctx);
    let Some(preset) = dirs.find(name)? else {
        return Err(unknown(name, &dirs));
    };
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "name": preset.name,
            "source": preset.source.kind(),
            "path": preset.source.path().map(|path| path.display().to_string()),
            "text": preset.text,
        }));
    } else {
        ctx.out
            .note(format_args!("preset {} ({})", preset.name, preset.source));
        ctx.out.text(preset.text.trim_end());
    }
    Ok(0)
}

fn eject(ctx: &Context, args: &PresetEjectArgs) -> Result<u8, AppError> {
    let (dirs, workspace) = dirs(ctx);
    let Some(text) = presets::built_in(&args.name) else {
        let message = match dirs.find(&args.name)? {
            Some(found) => format!(
                "{:?} is not a built-in preset; it is already a file you can edit: {}",
                args.name, found.source
            ),
            None => return Err(unknown(&args.name, &dirs)),
        };
        return Err(AppError::config(message));
    };
    let (dir, scope) = if args.user {
        (dirs.user.clone(), "user")
    } else {
        let Some(workspace) = workspace else {
            return Err(AppError::new(
                "workspace",
                "not inside a Git repository; run this in the project, or pass --user to \
                 copy the preset to the user's presets directory",
            ));
        };
        (workspace.root.join(PROJECT_PRESETS_DIR), "project")
    };
    let path = dir.join(format!("{}.toml", args.name));
    let replaced = path.exists();
    if replaced && !args.force {
        return Err(AppError::config(format!(
            "{} already exists; edit it, or pass --force to replace it with the built-in",
            path.display()
        )));
    }
    fs::create_dir_all(&dir)
        .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))?;
    fs::write(&path, ejected_text(&args.name, text, scope))
        .map_err(|error| AppError::io(format!("cannot write {}", path.display()), error))?;
    // A project file of the same name still wins over a user one.
    let shadowed_by = if scope == "user" {
        dirs.candidates(&args.name)?
            .into_iter()
            .find(|file| matches!(file.source, Source::Project(_)))
            .map(|file| file.source.to_string())
    } else {
        None
    };
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "name": args.name,
            "scope": scope,
            "path": path.display().to_string(),
            "replaced": replaced,
            "shadowed_by": shadowed_by,
        }));
    } else {
        ctx.out.text(format_args!(
            "copied the built-in {} preset to {}; edit it there",
            args.name,
            path.display()
        ));
        if let Some(by) = shadowed_by {
            ctx.out.note(format_args!(
                "this project uses its own copy instead ({by})"
            ));
        }
        ctx.out
            .text("`boatparty config show --resolved --origin` shows the effect");
    }
    Ok(0)
}

/// The built-in's text with a note under its summary line saying what the
/// copy is.
fn ejected_text(name: &str, text: &str, scope: &str) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let applies = if scope == "user" {
        "every project of this user that names it (unless the project has its own copy)"
    } else {
        "this project"
    };
    format!(
        "{first}\n\
         #\n\
         # Copied from boatparty {version}'s built-in {name:?} preset by `boatparty preset\n\
         # eject`. This file replaces the built-in for {applies}. Edit it as plain\n\
         # text; delete it to go back to the built-in.\n\
         {rest}",
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn unknown(name: &str, dirs: &PresetDirs) -> AppError {
    let known = dirs.names().unwrap_or_default().join(", ");
    AppError::new(
        "unknown_preset",
        format!("no preset named {name:?} (available: {known})"),
    )
}
