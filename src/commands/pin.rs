//! `boatparty release` and `boatparty assign`: steer which sandbox serves
//! this workspace. Both only change local records; neither talks to the
//! provider nor stops anything.

use serde_json::json;

use crate::Context;
use crate::cli::AssignArgs;
use crate::error::AppError;
use crate::pool::{acquire, workspace_exists};
use crate::project::Workspace;
use crate::state::{Member, Store};
use crate::system;

fn named(member: &Member) -> serde_json::Value {
    json!({ "name": member.name, "id": member.id })
}

/// Drop this workspace's affinity so another workspace may use its member.
pub fn release(ctx: &Context) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let name = project.name();
    let key = workspace.key();
    let released = Store::new(&ctx.paths).update(|state| acquire::release(state, name, &key))?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": name,
            "workspace": key,
            "released": released.is_some(),
            "member": released.as_ref().map(named),
        }));
        return Ok(0);
    }
    match released {
        Some(member) => ctx.out.text(format_args!(
            "released {} ({}); another workspace may use it now",
            member.name,
            member.id.as_deref().unwrap_or("pending")
        )),
        None => ctx.out.text(format_args!(
            "this workspace holds no sandbox of project {name}"
        )),
    }
    Ok(0)
}

/// Pin a free member of this project to this workspace.
pub fn assign(ctx: &Context, args: &AssignArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let name = project.name();
    let key = workspace.key();
    let assigned = Store::new(&ctx.paths).update(|state| {
        acquire::assign(
            state,
            name,
            &key,
            &args.member,
            &workspace_exists,
            system::now_ms(),
        )
    })?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": name,
            "workspace": key,
            "member": named(&assigned.member),
            "previous": assigned.previous.as_ref().map(named),
            "unchanged": assigned.unchanged,
        }));
        return Ok(0);
    }
    if assigned.unchanged {
        ctx.out.text(format_args!(
            "{} already serves this workspace",
            assigned.member.name
        ));
        return Ok(0);
    }
    ctx.out.text(format_args!(
        "{} ({}) now serves this workspace",
        assigned.member.name,
        assigned.member.id.as_deref().unwrap_or("pending")
    ));
    if let Some(previous) = &assigned.previous {
        ctx.out.note(format_args!(
            "{} is free now; another workspace may use it",
            previous.name
        ));
    }
    Ok(0)
}
