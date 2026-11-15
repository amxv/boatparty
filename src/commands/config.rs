//! `boatparty config show|validate`: the project's configuration as written
//! or as it takes effect, and a check of it and of every preset file.

use serde_json::{Value, json};

use crate::Context;
use crate::cli::{ConfigAction, ConfigArgs, ConfigShowArgs};
use crate::config::presets::{PresetDirs, PresetFile};
use crate::config::{self, Resolved, render};
use crate::error::AppError;
use crate::project::Workspace;

pub fn run(ctx: &Context, args: &ConfigArgs) -> Result<u8, AppError> {
    match &args.action {
        ConfigAction::Show(show) => self::show(ctx, show),
        ConfigAction::Validate => validate(ctx),
    }
}

fn show(ctx: &Context, args: &ConfigShowArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let resolved = config::load(&workspace.root, &ctx.paths)?;
    let effective = args.resolved || args.origin;
    let table = if effective {
        &resolved.merged
    } else {
        &resolved.own
    };
    if ctx.out.is_json() {
        let mut document = json!({
            "ok": true,
            "file": resolved.file.display().to_string(),
            "project": resolved.config.project.name,
            "view": if effective { "resolved" } else { "file" },
            "presets": presets_json(&resolved.presets),
            "config": serde_json::to_value(table).unwrap_or(Value::Null),
        });
        if effective {
            document["switched_off"] = resolved
                .switched_off
                .iter()
                .map(|off| json!({"section": off.section, "name": off.name, "by": off.by.to_string()}))
                .collect();
        }
        if args.origin {
            document["origins"] = resolved
                .origins
                .iter()
                .map(|(path, origin)| (path.clone(), json!(origin.to_string())))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }
        ctx.out.document(&document);
        return Ok(0);
    }
    let mut text = String::new();
    if effective {
        text.push_str(&format!(
            "# The effective configuration of project {}: boatparty's defaults, then {}, then {}.\n",
            resolved.config.project.name,
            layer_list(&resolved),
            resolved.file.display()
        ));
        for off in &resolved.switched_off {
            text.push_str(&format!(
                "# The {} step {:?} is switched off by {}.\n",
                off.section, off.name, off.by
            ));
        }
        text.push('\n');
    }
    text.push_str(&render::render(
        table,
        args.origin.then_some(&resolved.origins),
    ));
    ctx.out.text(text.trim_end());
    Ok(0)
}

fn validate(ctx: &Context) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let resolved = config::load(&workspace.root, &ctx.paths)?;
    let dirs = PresetDirs::new(ctx.paths.user_presets_dir(), Some(&workspace.root));
    let files = dirs.files_on_disk()?;
    let problems: Vec<String> = files
        .iter()
        .filter_map(|file| config::check_preset(file).err())
        .map(|error| error.to_string())
        .collect();
    if !problems.is_empty() {
        return Err(AppError::config(problems.join("\n")));
    }
    let checked: Vec<String> = files
        .iter()
        .filter_map(|file| file.source.path())
        .map(|path| path.display().to_string())
        .collect();
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "file": resolved.file.display().to_string(),
            "project": resolved.config.project.name,
            "presets": presets_json(&resolved.presets),
            "preset_files_checked": checked,
        }));
    } else {
        ctx.out.text(format_args!(
            "{} is valid: project {}, {}",
            resolved.file.display(),
            resolved.config.project.name,
            layer_list(&resolved)
        ));
        if !checked.is_empty() {
            ctx.out.text(format_args!(
                "checked {} preset file(s): {}",
                checked.len(),
                checked.join(", ")
            ));
        }
    }
    Ok(0)
}

fn layer_list(resolved: &Resolved) -> String {
    if resolved.presets.is_empty() {
        "no presets".to_string()
    } else {
        let names: Vec<String> = resolved
            .presets
            .iter()
            .map(|preset| format!("preset {} ({})", preset.name, preset.source))
            .collect();
        names.join(", ")
    }
}

pub(crate) fn presets_json(presets: &[PresetFile]) -> Value {
    presets
        .iter()
        .map(|preset| {
            json!({
                "name": preset.name,
                "source": preset.source.kind(),
                "path": preset.source.path().map(|path| path.display().to_string()),
            })
        })
        .collect()
}
