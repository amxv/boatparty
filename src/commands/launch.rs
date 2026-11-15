//! `boatparty launch`: bring this workspace's sandbox up and connect to it.

use serde_json::json;

use super::WorkspaceContext;
use super::status::member_json;
use crate::Context;
use crate::error::AppError;
use crate::pool::lease;

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let mut up = pool.bring_up(wctx.bring_up_options(true), None)?;
    // launch holds no lease: like the last command to finish, it leaves the
    // idle deadline to the detached helper (which respects live leases and
    // an unfinished hydration), so an unused member stops after idle_minutes.
    if let Some(id) = up.member.id.as_deref()
        && let Err(error) = lease::spawn_idle_helper(&pool.project, id)
    {
        ctx.out.note(format_args!("warning: {error}"));
    }
    // Read it once more so the reported auto-stop includes this launch's change.
    if let Ok(latest) = wctx.provider.get(&up.sandbox.id)
        && latest.state == up.sandbox.state
    {
        up.sandbox = latest;
    }
    let how = if up.created {
        "created"
    } else if up.reclaimed {
        "resumed on its kept machine"
    } else if up.resumed {
        "resumed on a new machine"
    } else {
        "already running"
    };
    if ctx.out.is_json() {
        let mut member = member_json(
            &ctx.paths,
            &up.member,
            Some(&up.sandbox),
            &wctx.workspace.key(),
        );
        member["created"] = json!(up.created);
        member["resumed"] = json!(up.resumed);
        member["reclaimed"] = json!(up.reclaimed);
        member["reconnected"] = json!(up.reconnected);
        ctx.out.document(&json!({
            "ok": true,
            "project": pool.project,
            "workspace": wctx.workspace.key(),
            "member": member,
        }));
    } else {
        ctx.out.text(format_args!(
            "{} ({}) {how}; auto-stop {}",
            up.member.name,
            up.sandbox.id,
            up.sandbox.auto_stop_at.as_deref().unwrap_or("off")
        ));
        ctx.out.text("enter it with `boatparty board`");
    }
    Ok(0)
}
