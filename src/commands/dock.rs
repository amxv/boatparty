//! `boatparty dock`: stop this workspace's sandbox, keeping its disk.
//!
//! A member whose restored files are still arriving is never stopped: dock
//! waits until the provider says hydration is complete. An unknown hydration
//! counts as incomplete.

use std::time::Duration;

use serde_json::json;

use super::WorkspaceContext;
use super::status::member_json;
use crate::Context;
use crate::cli::DockArgs;
use crate::error::AppError;
use crate::pool::wait::{self, Deadline};
use crate::provider::{Hydration, Provider, Sandbox, SandboxState};
use crate::transport::ssh::SshTarget;

const HYDRATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STOP_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub fn run(ctx: &Context, args: &DockArgs) -> Result<u8, AppError> {
    let wctx = WorkspaceContext::open(ctx)?;
    let pool = wctx.pool(ctx);
    let member = pool.resolve(args.member.as_deref())?;
    pool.refuse_if_leased(&member, "stop")?;
    let Some(id) = member.id.clone() else {
        return Err(AppError::new(
            "member_pending",
            format!(
                "{} is still being created; there is nothing to stop",
                member.name
            ),
        )
        .with_hint("run `boatparty launch` to finish creating it"));
    };
    let provider = wctx.provider.as_ref();

    let hydration = Deadline::after(HYDRATION_TIMEOUT);
    let mut said_waiting = false;
    let mut sandbox = get(provider, &id)?;
    let requested = loop {
        match sandbox.state {
            SandboxState::Gone => {
                return Err(AppError::new(
                    "not_found",
                    format!("{} ({id}) no longer exists at the provider", member.name),
                )
                .with_hint(format!(
                    "drop the record with `boatparty rm {}`",
                    member.name
                )));
            }
            SandboxState::Error => {
                return Err(AppError::new(
                    "member_failed",
                    format!("{} ({id}) is in the error state", member.name),
                )
                .with_hint(format!("delete it with `boatparty rm {}`", member.name)));
            }
            SandboxState::Stopped | SandboxState::Stopping => break false,
            SandboxState::Running if sandbox.hydration.is_complete() => {
                SshTarget::for_sandbox(&ctx.paths, &id).close_master();
                wait::retry(Duration::from_secs(60), wait::transient, || {
                    provider.stop(&id)
                })?;
                break true;
            }
            SandboxState::Running | SandboxState::Starting => {
                if !said_waiting {
                    ctx.out.note(format_args!(
                        "waiting for {} to {} before stopping it",
                        member.name,
                        waiting_for(&sandbox)
                    ));
                    said_waiting = true;
                }
                if !hydration.tick() {
                    return Err(AppError::new(
                        "hydration_timeout",
                        format!(
                            "{} ({id}) did not report its files complete within {} minutes; \
                             not stopping it",
                            member.name,
                            HYDRATION_TIMEOUT.as_secs() / 60
                        ),
                    ));
                }
                sandbox = get(provider, &id)?;
            }
        }
    };

    if args.wait {
        let deadline = Deadline::after(STOP_TIMEOUT);
        sandbox = get(provider, &id)?;
        while sandbox.state != SandboxState::Stopped {
            if sandbox.state == SandboxState::Gone || !deadline.tick() {
                return Err(AppError::new(
                    "stop_timeout",
                    format!(
                        "{} ({id}) is {} rather than stopped",
                        member.name, sandbox.state
                    ),
                ));
            }
            sandbox = get(provider, &id)?;
        }
    } else if requested {
        sandbox.state = SandboxState::Stopping;
    }

    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "project": pool.project,
            "stop_requested": requested,
            "member": member_json(&ctx.paths, &member, Some(&sandbox), &wctx.workspace.key()),
        }));
    } else {
        let word = if requested { "stopping" } else { "already" };
        ctx.out.text(format_args!(
            "{} ({id}) {}",
            member.name,
            match (sandbox.state, word) {
                (SandboxState::Stopped, _) => "stopped".to_string(),
                (state, "already") => format!("was already {state}"),
                (state, _) => state.to_string(),
            }
        ));
    }
    Ok(0)
}

fn get(provider: &dyn Provider, id: &str) -> Result<Sandbox, AppError> {
    Ok(wait::retry(
        Duration::from_secs(60),
        wait::transient,
        || provider.get(id),
    )?)
}

fn waiting_for(sandbox: &Sandbox) -> String {
    match (sandbox.state, sandbox.hydration) {
        (SandboxState::Starting, _) => "finish starting".into(),
        (
            _,
            Hydration::InProgress {
                percent: Some(percent),
            },
        ) => {
            format!("finish restoring its files ({percent}%)")
        }
        (_, Hydration::InProgress { percent: None }) => "finish restoring its files".into(),
        _ => "report that its files are complete".into(),
    }
}
