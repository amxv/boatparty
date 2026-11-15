//! `boatparty rm`: delete a member at the provider and drop its record.
//!
//! Deletion is asynchronous at the provider; rm waits for the deletion
//! operation to complete before it drops the record.

use std::time::Duration;

use serde_json::json;

use super::WorkspaceContext;
use crate::Context;
use crate::cli::RmArgs;
use crate::error::AppError;
use crate::pool::LABEL_PROJECT;
use crate::pool::remove::delete_sandbox;
use crate::pool::wait;
use crate::provider::SandboxState;

pub fn run(ctx: &Context, args: &RmArgs) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let provider = wctx.provider.as_ref();
    let member = pool.resolve(args.member.as_deref())?;
    pool.refuse_if_leased(&member, "delete")?;

    // A pending record may have a sandbox whose create answer was lost.
    let id = match member.id.clone() {
        Some(id) => Some(id),
        None => wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.list_by_label(LABEL_PROJECT, &pool.project)
        })?
        .into_iter()
        .find(|sandbox| {
            sandbox.name.as_deref() == Some(member.name.as_str())
                && sandbox.state != SandboxState::Gone
        })
        .map(|sandbox| sandbox.id),
    };

    let deletion = match &id {
        Some(id) => delete_sandbox(&ctx.paths, provider, ctx.out, &member.name, id)?,
        None => None,
    };
    pool.forget(&member)?;

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": pool.project,
            "member": { "name": member.name, "id": id },
            "deleted": deletion.is_some(),
            "deletion": deletion.as_ref().map(|deletion| json!({
                "operation_id": deletion.operation_id,
                "status": deletion.status.as_str(),
            })),
        }));
    } else {
        match (&id, &deletion) {
            (Some(id), Some(_)) => ctx.out.text(format_args!("deleted {} ({id})", member.name)),
            _ => ctx
                .out
                .text(format_args!("dropped the record of {}", member.name)),
        }
    }
    Ok(0)
}
