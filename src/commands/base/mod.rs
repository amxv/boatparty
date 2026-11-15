//! `boatparty base build|status|rm`: the project's base snapshot.

pub mod build;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::WorkspaceContext;
use crate::Context;
use crate::base::staleness::{self, Staleness, short_ref};
use crate::base::{self, BaseRecord};
use crate::cli::{BaseAction, BaseArgs, BaseRmArgs};
use crate::error::AppError;
use crate::pool::LABEL_PROJECT;
use crate::pool::remove::delete_sandbox;
use crate::pool::wait;
use crate::project::{ProjectConfig, Workspace};
use crate::provider::{SandboxState, SavedSnapshot, SnapshotAllowance};
use crate::signals::Interrupts;
use crate::state::{Member, Role, Store};
use crate::sync::WorktreeInfo;
use crate::system;
use build::{phases_json, round, short};

pub fn run(ctx: &Context, args: &BaseArgs) -> Result<u8, AppError> {
    match &args.action {
        BaseAction::Build => run_build(ctx),
        BaseAction::Status => status(ctx),
        BaseAction::Rm(args) => rm(ctx, args),
    }
}

fn run_build(ctx: &Context) -> Result<u8, AppError> {
    let started = std::time::Instant::now();
    let wctx = WorkspaceContext::open(ctx)?;
    let interrupts = Interrupts::install()?;
    let built = match build::build(ctx, &wctx, &interrupts) {
        Ok(built) => built,
        Err(error) => {
            if let Some(status) = interrupts.exit_status() {
                ctx.out.error(&error);
                return Ok(status);
            }
            return Err(error);
        }
    };
    let seconds = round(started.elapsed().as_secs_f64());
    let record = &built.record;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": wctx.project.name(),
            "base": record.to_value(),
            "builder": {
                "name": built.builder.name,
                "id": built.builder.id,
                "created": built.up.0,
                "resumed": built.up.1,
            },
            "pack_bytes": built.pack_bytes,
            "seed_route": built.seed_route,
            "phases": phases_json(&built.phases),
            "seconds": seconds,
        }));
    } else {
        ctx.out.text(format_args!(
            "saved the base {}: {} at {}{} in {seconds:.0}s; new sandboxes of {} start from it",
            record.snapshot,
            short_ref(&built.base_ref.name),
            short(&record.commit),
            record
                .size_bytes
                .map(|bytes| format!(" ({})", size(bytes)))
                .unwrap_or_default(),
            wctx.project.name(),
        ));
    }
    Ok(0)
}

/// The project's base record and builder from `state.json`.
fn recorded(
    ctx: &Context,
    project: &str,
) -> Result<(Option<BaseRecord>, Option<Member>), AppError> {
    let state = Store::new(&ctx.paths).load()?;
    let pool = state.project(project);
    Ok((
        pool.and_then(base::record),
        pool.and_then(|pool| {
            pool.members
                .iter()
                .find(|member| member.role == Role::Builder)
                .cloned()
        }),
    ))
}

fn status(ctx: &Context) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project(&ctx.paths)?;
    let name = project.name();
    let snapshot_name = base::snapshot_name(name);
    let (record, builder) = recorded(ctx, name)?;
    let staleness = record
        .as_ref()
        .map(|record| assess(&project, &workspace.root, record))
        .transpose()?;
    let provider = super::open_provider(ctx)?;
    let (snapshots, allowance) = wait::retry(Duration::from_secs(60), wait::transient, || {
        provider.saved_snapshots(&snapshot_name)
    })?;
    let saved = snapshots
        .into_iter()
        .find(|snapshot| snapshot.name == snapshot_name);
    let now = system::now_ms();

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": name,
            "snapshot": snapshot_name,
            "base": record.as_ref().map(BaseRecord::to_value),
            "built_at": record.as_ref().map(|record| crate::runs::rfc3339(record.built_at)),
            "saved": saved.as_ref().map(saved_json),
            "staleness": staleness,
            "stale": staleness.as_ref().map(|staleness| staleness.stale),
            "allowance": allowance.as_ref().map(allowance_json),
            "builder": builder.as_ref().map(|builder| json!({
                "name": builder.name,
                "id": builder.id,
            })),
        }));
        return Ok(0);
    }
    let Some(record) = &record else {
        ctx.out.text(format_args!("project {name} has no base"));
        if let Some(saved) = &saved {
            ctx.out.text(format_args!(
                "a saved snapshot {} ({}) exists at the provider but is not recorded here; \
                 `boatparty base build` replaces it, `boatparty base rm` deletes it",
                saved.name,
                saved.status.as_str()
            ));
        } else {
            ctx.out.text(
                "new sandboxes build their dependencies on their first command; \
                 `boatparty base build` saves a base they start from instead",
            );
        }
        if let Some(allowance) = &allowance {
            ctx.out.text(format_args!(
                "saved snapshots: {}",
                allowance_words(allowance)
            ));
        }
        return Ok(0);
    };
    ctx.out.text(format_args!(
        "base {}: {} at {} ({} history), built {} ago{}",
        record.snapshot,
        record.git_ref.as_deref().map(short_ref).unwrap_or("?"),
        short(&record.commit),
        record.mode,
        age_words(now.saturating_sub(record.built_at)),
        saved
            .as_ref()
            .and_then(|saved| saved.size_bytes.or(record.size_bytes))
            .map(|bytes| format!(", {}", size(bytes)))
            .unwrap_or_default(),
    ));
    match &saved {
        None => ctx.out.text(
            "warning: the saved snapshot no longer exists at the provider; new sandboxes start \
             blank until `boatparty base build` runs",
        ),
        Some(saved) if saved.status != crate::provider::SnapshotStatus::Ready => {
            ctx.out.text(format_args!(
                "the saved snapshot is {}",
                saved.status.as_str()
            ));
        }
        Some(_) => {}
    }
    if let Some(staleness) = &staleness {
        let behind = match staleness.behind {
            Some(behind) => format!(
                "{behind} commits behind {}",
                staleness
                    .git_ref
                    .as_deref()
                    .map(short_ref)
                    .unwrap_or("its ref")
            ),
            None => "commits behind: unknown".to_string(),
        };
        let inputs = match &staleness.inputs_changed {
            Some(files) if files.is_empty() => "stale inputs unchanged".to_string(),
            Some(files) => format!("changed: {}", files.join(", ")),
            None => "stale inputs: unknown".to_string(),
        };
        ctx.out.text(format_args!("{behind}; {inputs}"));
        if staleness.stale {
            ctx.out.text(format_args!(
                "stale: {}; `boatparty base build` refreshes it",
                staleness.reasons.join("; ")
            ));
        }
    }
    if let Some(allowance) = &allowance {
        ctx.out.text(format_args!(
            "saved snapshots: {}",
            allowance_words(allowance)
        ));
    }
    Ok(0)
}

fn assess(
    project: &ProjectConfig,
    root: &Path,
    record: &BaseRecord,
) -> Result<Staleness, AppError> {
    let info = WorktreeInfo::read(root)?;
    let config = project.config();
    Ok(staleness::assess(
        &info,
        record,
        config.base.git_ref.as_deref(),
        &config.base.stale_inputs,
        system::now_ms(),
    ))
}

fn rm(ctx: &Context, args: &BaseRmArgs) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let provider = wctx.provider.as_ref();
    let name = pool.project.clone();
    let snapshot_name = base::snapshot_name(&name);
    let (record, builder) = recorded(ctx, &name)?;
    if let Some(builder) = &builder {
        pool.refuse_if_leased(builder, "remove the base while a build uses")?;
    }
    let deleted_snapshot = wait::retry(Duration::from_secs(60), wait::transient, || {
        provider.delete_saved_snapshot(&snapshot_name)
    })?;
    pool.store.update(|state| {
        state.project_mut(&name).base = None;
        Ok(())
    })?;
    let mut builder_deleted = None;
    if let (Some(builder), false) = (&builder, args.keep_builder) {
        let id = match builder.id.clone() {
            Some(id) => Some(id),
            None => wait::retry(Duration::from_secs(60), wait::transient, || {
                provider.list_by_label(LABEL_PROJECT, &name)
            })?
            .into_iter()
            .find(|sandbox| {
                sandbox.name.as_deref() == Some(builder.name.as_str())
                    && sandbox.state != SandboxState::Gone
            })
            .map(|sandbox| sandbox.id),
        };
        if let Some(id) = &id {
            delete_sandbox(&ctx.paths, provider, ctx.out, &builder.name, id)?;
        }
        pool.forget(builder)?;
        builder_deleted = Some(json!({"name": builder.name, "id": id}));
    }

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": name,
            "snapshot": snapshot_name,
            "snapshot_deleted": deleted_snapshot,
            "record_removed": record.is_some(),
            "builder_deleted": builder_deleted,
        }));
    } else {
        ctx.out.text(if deleted_snapshot {
            format!("deleted the saved snapshot {snapshot_name}")
        } else {
            format!("no saved snapshot {snapshot_name} existed")
        });
        if let Some(builder) = &builder_deleted {
            ctx.out.text(format_args!(
                "deleted the builder {}",
                builder["name"].as_str().unwrap_or_default()
            ));
        }
        ctx.out
            .text("new sandboxes now start blank; existing ones are unchanged");
    }
    Ok(0)
}

/// What `status` and `init` say about the base, from local knowledge only:
/// `None` when there is nothing worth saying (no base and nothing a base
/// would warm, or a fresh base).
pub fn summary(ctx: &Context, project: &ProjectConfig, root: &Path) -> (Value, Option<String>) {
    let name = project.name();
    let config = project.config();
    let record = recorded(ctx, name).ok().and_then(|(record, _)| record);
    let Some(record) = record else {
        let useful = config.base.warm.is_some() || !config.ensure.is_empty();
        let note = useful.then(|| {
            format!(
                "project {name} has no base, so each new sandbox builds its dependencies \
                 first; `boatparty base build` saves one"
            )
        });
        return (
            json!({"present": false, "stale": null, "reasons": []}),
            note,
        );
    };
    let staleness = assess(project, root, &record).ok();
    let reasons = staleness
        .as_ref()
        .map(|staleness| staleness.reasons.clone())
        .unwrap_or_default();
    let stale = staleness.as_ref().map(|staleness| staleness.stale);
    let note = (stale == Some(true)).then(|| {
        format!(
            "the base {} is stale ({}); `boatparty base build` refreshes it",
            record.snapshot,
            reasons.join("; ")
        )
    });
    (
        json!({
            "present": true,
            "snapshot": record.snapshot,
            "commit": record.commit,
            "built_at": crate::runs::rfc3339(record.built_at),
            "stale": stale,
            "reasons": reasons,
        }),
        note,
    )
}

fn saved_json(saved: &SavedSnapshot) -> Value {
    json!({
        "name": saved.name,
        "status": saved.status.as_str(),
        "error": saved.error,
        "size_bytes": saved.size_bytes,
        "created_at": saved.created_at,
        "source_id": saved.source_id,
    })
}

fn allowance_json(allowance: &SnapshotAllowance) -> Value {
    json!({
        "included": allowance.included,
        "used": allowance.used,
        "extra": allowance.extra,
        "extra_monthly_dollars": allowance.extra_monthly_dollars,
        "price_per_extra_daily_dollars": allowance.price_per_extra_daily_dollars,
    })
}

fn allowance_words(allowance: &SnapshotAllowance) -> String {
    let count = |value: Option<u64>| value.map_or("?".to_string(), |value| value.to_string());
    let mut words = format!(
        "{} used of {} included",
        count(allowance.used),
        count(allowance.included)
    );
    if let Some(extra) = allowance.extra
        && extra > 0
    {
        words.push_str(&format!(
            ", {extra} extra{}",
            allowance
                .extra_monthly_dollars
                .map(|dollars| format!(" (${dollars:.2} a month)"))
                .unwrap_or_default()
        ));
    }
    words
}

fn age_words(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "less than a minute".to_string(),
        m if m < 120 => format!("{m} min"),
        m if m < 48 * 60 => format!("{} h", m / 60),
        m => format!("{} days", m / (24 * 60)),
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
        b => format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}
