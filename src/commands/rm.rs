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
use crate::pool::wait::{self, Deadline};
use crate::provider::{DeletionStatus, ErrorKind, SandboxState};
use crate::transport::ssh::SshTarget;

const DELETE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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

    let mut deletion = None;
    if let Some(id) = &id {
        let target = SshTarget::for_sandbox(&ctx.paths, id);
        target.close_master();
        match wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.delete(id)
        }) {
            Ok(accepted) => {
                ctx.out
                    .note(format_args!("deleting {} ({id})", member.name));
                let deadline = Deadline::after(DELETE_TIMEOUT);
                let mut current = accepted;
                while current.status != DeletionStatus::Completed {
                    if !deadline.tick() {
                        return Err(AppError::new(
                            "deletion_timeout",
                            format!(
                                "the provider accepted the deletion of {} ({id}) but it is still \
                                 {} after {} minutes",
                                member.name,
                                current.status.as_str(),
                                DELETE_TIMEOUT.as_secs() / 60
                            ),
                        )
                        .with_hint("run `boatparty rm` again later to finish"));
                    }
                    current = wait::retry(Duration::from_secs(60), wait::transient, || {
                        provider.deletion(&current.operation_id)
                    })?;
                }
                deletion = Some(current);
            }
            Err(error) if matches!(error.kind(), ErrorKind::NotFound) => {
                ctx.out.note(format_args!(
                    "{} ({id}) was already gone at the provider",
                    member.name
                ));
            }
            Err(error) => return Err(error.into()),
        }
        target.remove_files();
    }
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
