//! `boatparty board`: a shell, or one command, in the sandbox's checkout.
//!
//! When the workspace's member has a live SSH master, nothing but `git` (to
//! find the workspace), a local lease write and `ssh` runs before the
//! command: no provider call, so scripts can call it once per action. The
//! `session` lease's heartbeat runs in the background, and the idle deadline
//! is set by a detached helper after the last lease ends.

use std::io::IsTerminal;
use std::os::unix::process::ExitStatusExt;

use super::WorkspaceContext;
use crate::Context;
use crate::cli::BoardArgs;
use crate::error::AppError;
use crate::pool::lease::{self, Heartbeat, LeaseTimes};
use crate::pool::{remote_checkout, warm_member};
use crate::project::Workspace;
use crate::signals::Interrupts;
use crate::state::{LeaseKind, Store};
use crate::system;
use crate::transport::ssh::{SshTarget, shell_quote};

pub fn run(ctx: &Context, args: &BoardArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let name = project.name();
    let (member, target) = match warm_member(&ctx.paths, name, &workspace.key())? {
        Some(warm) => warm,
        None => {
            let wctx = WorkspaceContext::open(ctx)?;
            let up = wctx.pool(ctx).bring_up(wctx.bring_up_options(false))?;
            (up.member, up.target)
        }
    };
    let account = super::load_account(ctx)?;
    let times = LeaseTimes::from_defaults(&account.config.defaults);
    let store = Store::new(&ctx.paths);
    let label = if args.command.is_empty() {
        "shell".to_string()
    } else {
        args.command.join(" ")
    };
    lease::register(&store, name, &member, LeaseKind::Session, &label)?;
    // SIGINT and SIGTERM reach ssh, which ends; this process then lets go of
    // its lease before exiting.
    let interrupts = Interrupts::install()?;
    let id = member.id.clone().unwrap_or_default();
    let mut heartbeat = Heartbeat::for_member(&ctx.paths, &id, times);
    let result = exec(&target, name, &args.command);
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
    let code = result?;
    Ok(interrupts.exit_status().unwrap_or(code))
}

/// The remote command line: enter the checkout, then run `argv` (quoted
/// word by word) or a login shell.
pub fn remote_command(project: &str, argv: &[String]) -> String {
    let dir = shell_quote(&remote_checkout(project));
    let prefix = format!("mkdir -p {dir} && cd {dir} && ");
    if argv.is_empty() {
        format!("{prefix}exec \"${{SHELL:-/bin/bash}}\" -l")
    } else {
        let words: Vec<String> = argv.iter().map(|word| shell_quote(word)).collect();
        format!("{prefix}{}", words.join(" "))
    }
}

fn exec(target: &SshTarget, project: &str, argv: &[String]) -> Result<u8, AppError> {
    let interactive = argv.is_empty() && std::io::stdin().is_terminal();
    let mut command = target.ssh();
    command
        .arg(if interactive { "-t" } else { "-T" })
        .arg(&target.alias)
        .arg(remote_command(project, argv));
    let status = command
        .status()
        .map_err(|error| AppError::io("cannot run ssh", error))?;
    let code = match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => 255,
    };
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(255))
}

#[cfg(test)]
mod tests {
    use super::remote_command;

    #[test]
    fn commands_run_in_the_checkout_with_every_word_quoted() {
        assert_eq!(
            remote_command("app", &["uname".into(), "-a".into()]),
            "mkdir -p /home/user/app && cd /home/user/app && uname -a"
        );
        assert_eq!(
            remote_command(
                "app",
                &["sh".into(), "-c".into(), "echo $HOME; exit 3".into()]
            ),
            "mkdir -p /home/user/app && cd /home/user/app && sh -c 'echo $HOME; exit 3'"
        );
        assert!(remote_command("app", &[]).ends_with("exec \"${SHELL:-/bin/bash}\" -l"));
    }
}
