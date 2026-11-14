//! `boatparty board`: a shell, or one command, in the sandbox's checkout.
//!
//! When the workspace's member has a live SSH master, nothing but `git` (to
//! find the workspace) and `ssh` runs: no provider call and no state write,
//! so scripts can call it once per action.

use std::io::IsTerminal;
use std::os::unix::process::ExitStatusExt;

use super::WorkspaceContext;
use crate::Context;
use crate::cli::BoardArgs;
use crate::error::AppError;
use crate::pool::{remote_checkout, warm_member};
use crate::project::Workspace;
use crate::transport::ssh::{SshTarget, shell_quote};

pub fn run(ctx: &Context, args: &BoardArgs) -> Result<u8, AppError> {
    let workspace = Workspace::current()?;
    let project = workspace.load_project()?;
    let target = match warm_member(&ctx.paths, project.name(), &workspace.key())? {
        Some((_, target)) => target,
        None => {
            let wctx = WorkspaceContext::open(ctx)?;
            wctx.pool(ctx)
                .bring_up(wctx.bring_up_options(false))?
                .target
        }
    };
    exec(&target, project.name(), &args.command)
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
