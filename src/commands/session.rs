//! A `session` lease around work on this workspace's member that does not
//! sync: `board` and `fetch`.
//!
//! When the member has a live SSH master, nothing but `git` (to find the
//! workspace), a local lease write and the work itself runs: no provider
//! call. The heartbeat runs in the background, and the idle deadline is set
//! by a detached helper after the last lease ends.

use super::WorkspaceContext;
use crate::Context;
use crate::error::AppError;
use crate::pool::lease::{self, Heartbeat, LeaseTimes};
use crate::pool::{LeaseRequest, warm_member};
use crate::project::Workspace;
use crate::signals::Interrupts;
use crate::state::{LeaseKind, Member, Store};
use crate::system;
use crate::transport::ssh::SshTarget;

/// What the work receives.
pub(crate) struct Held<'a> {
    pub project: &'a str,
    pub member: &'a Member,
    pub target: &'a SshTarget,
}

/// The work's result, and the exit status a SIGINT or SIGTERM during it
/// asks for (which wins over the result: an interrupted child usually fails).
pub(crate) struct Ended<T> {
    pub value: Result<T, AppError>,
    pub interrupted: Option<u8>,
}

/// Bring this workspace's member up if needed, hold a `session` lease
/// labelled `label` while `work` runs, then let go.
pub(crate) fn with_session<T>(
    ctx: &Context,
    label: &str,
    work: impl FnOnce(&Held<'_>) -> Result<T, AppError>,
) -> Result<Ended<T>, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let name = project.name();
    let store = Store::new(&ctx.paths);
    let (member, target) = connect(ctx, &store, name, &workspace, LeaseKind::Session, label)?;
    let account = super::load_account(ctx)?;
    let times = LeaseTimes::from_defaults(&account.config.defaults);
    // SIGINT and SIGTERM reach the child processes, which end; this process
    // then lets go of its lease before exiting.
    let interrupts = Interrupts::install()?;
    let id = member.id.clone().unwrap_or_default();
    let mut heartbeat = Heartbeat::for_member(&ctx.paths, &id, times);
    let result = work(&Held {
        project: name,
        member: &member,
        target: &target,
    });
    heartbeat.stop();
    let released = lease::finish(
        &store,
        name,
        &id,
        |member| member.last_used_at = system::now_ms(),
        || {
            if let Err(error) = lease::spawn_idle_helper(name, &id) {
                ctx.out.note(format_args!("warning: {error}"));
            }
        },
    );
    if let Err(error) = released {
        ctx.out.note(format_args!(
            "warning: could not release the lease: {error}"
        ));
    }
    Ok(Ended {
        value: result,
        interrupted: interrupts.exit_status(),
    })
}

/// This workspace's member with a lease of `kind` held on it: through a
/// live SSH master with no provider call when possible, else by acquiring
/// and bringing it up (which takes the lease in the same state write).
pub(crate) fn connect(
    ctx: &Context,
    store: &Store,
    project: &str,
    workspace: &Workspace,
    kind: LeaseKind,
    label: &str,
) -> Result<(Member, SshTarget), AppError> {
    let key = workspace.key();
    if let Some((member, target)) = warm_member(&ctx.paths, project, &key)?
        && lease::register(store, project, &key, &member, kind, label)?
    {
        return Ok((member, target));
    }
    let wctx = WorkspaceContext::open(ctx)?;
    let up = wctx.pool(ctx).bring_up(
        wctx.bring_up_options(false),
        Some(&LeaseRequest::new(kind, label)),
    )?;
    Ok((up.member, up.target))
}
