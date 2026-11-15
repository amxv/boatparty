//! `boatparty board`: a shell, or one command, in the sandbox's checkout.
//!
//! When the workspace's member has a live SSH master, nothing but `git` (to
//! find the workspace), a local lease write and `ssh` runs before the
//! command: no provider call, so scripts can call it once per action. The
//! `session` lease's heartbeat runs in the background, and the idle deadline
//! is set by a detached helper after the last lease ends.
//!
//! The shell or command gets what `cheers` gives a command: the Git
//! identity, the GitHub token when `[run] github_token` is on, `[run.env]`
//! and `BOATPARTY=1` (see `sync::hold`). Nothing is synced and no step runs.
//!
//! With `--json`, a command's output travels as the frames `cheers` uses
//! (stdout, stderr, then one result frame); an interactive shell has no
//! JSON form.

use std::io::{IsTerminal, Read};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::mpsc;

use super::session::{Held, with_session};
use crate::Context;
use crate::cli::BoardArgs;
use crate::error::AppError;
use crate::runner::{RunnerHost, RunnerSpec, SshRunner, remote_invocation_in};
use crate::stream::Stream;
use crate::sync::{self, Hold, RunCredentials, Sink, TokenSource};
use crate::transport::ssh::{SshTarget, shell_quote};

/// The remote command line for `argv`, and the hold that serves its
/// credentials (kept until the command ends).
struct Prepared {
    line: String,
    _hold: Option<Hold>,
    git_identity: bool,
    github_token: bool,
}

/// Gather the credentials, start a hold for them and build the `board`
/// invocation. When the hold cannot start, the command still runs, as a
/// plain command in the checkout without the credentials or `[run.env]`.
fn prepare(held: &Held<'_>, argv: &[String], note: &dyn Fn(&str)) -> Prepared {
    let config = held.config;
    let token_source = config.run_token_source();
    // Read only now that the command is about to run; held in memory.
    let credentials =
        RunCredentials::gather(&held.workspace.root, config.git_identity(), &token_source);
    if token_source != TokenSource::None && credentials.github_token.is_none() {
        note(
            "[run] github_token is on but no GitHub token was found (set \
             BOATPARTY_GITHUB_TOKEN or run `gh auth login`); the session runs without one",
        );
    }
    let host = SshRunner {
        target: held.target.clone(),
        upload: None,
        spec: Some(RunnerSpec::new(config.config())),
        dedicated: None,
    };
    match sync::hold(&host, held.project, &credentials) {
        Ok(hold) => {
            let args = hold.board_args(held.project, host.spec_hash(), &credentials, argv);
            Prepared {
                line: remote_invocation_in(Some(&config.remote_checkout()), &args),
                github_token: hold.socket.is_some(),
                git_identity: credentials.has_identity(),
                _hold: Some(hold),
            }
        }
        Err(error) => {
            note(&format!(
                "warning: {error}; running without the project's credentials and [run.env]"
            ));
            Prepared {
                line: remote_command(&config.remote_checkout(), argv),
                _hold: None,
                git_identity: false,
                github_token: false,
            }
        }
    }
}

pub fn run(ctx: &Context, args: &BoardArgs) -> Result<u8, AppError> {
    if ctx.out.is_json() {
        return run_json(ctx, args);
    }
    let label = if args.command.is_empty() {
        "shell".to_string()
    } else {
        args.command.join(" ")
    };
    let out = &ctx.out;
    let ended = with_session(ctx, &label, |held| {
        let note = |message: &str| out.note(format_args!("{message}"));
        let prepared = prepare(held, &args.command, &note);
        exec(held.target, &prepared.line, args.command.is_empty())
    })?;
    let code = ended.value?;
    Ok(ended.interrupted.unwrap_or(code))
}

/// `board --json -- COMMAND`: the command's output as frames, then a result.
fn run_json(ctx: &Context, args: &BoardArgs) -> Result<u8, AppError> {
    let mut stream = Stream::new(true);
    if args.command.is_empty() {
        let error = AppError::new(
            "usage",
            "an interactive shell has no JSON form; pass a command after --",
        );
        stream.error(&error);
        return Ok(error.exit_code());
    }
    let began = std::time::Instant::now();
    let label = args.command.join(" ");
    let ended = with_session(ctx, &label, |held| {
        let notes = std::cell::RefCell::new(Vec::new());
        let note = |message: &str| notes.borrow_mut().push(message.to_string());
        let prepared = prepare(held, &args.command, &note);
        for message in notes.into_inner() {
            stream.note(message);
        }
        let code = exec_framed(held.target, &prepared.line, &mut stream)?;
        Ok((
            code,
            held.project.to_string(),
            held.member.name.clone(),
            held.member.id.clone().unwrap_or_default(),
            prepared.git_identity,
            prepared.github_token,
        ))
    });
    let (code, project, member, id, git_identity, github_token) = match ended.and_then(|ended| {
        let value = ended.value?;
        Ok((value, ended.interrupted))
    }) {
        Ok((value, interrupted)) => {
            let (code, project, member, id, git_identity, github_token) = value;
            (
                interrupted.unwrap_or(code),
                project,
                member,
                id,
                git_identity,
                github_token,
            )
        }
        Err(error) => {
            stream.error(&error);
            return Ok(error.exit_code());
        }
    };
    stream.result(serde_json::json!({
        "ok": true,
        "kind": "board",
        "exit_code": code,
        "project": project,
        "member": { "name": member, "id": id },
        "command": remote_words(&args.command),
        "git_identity": git_identity,
        "github_token": github_token,
        "seconds": (began.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
    }));
    Ok(code)
}

/// The plain remote command line, used when the runner cannot prepare the
/// session: enter the checkout, then run `argv` (quoted word by word) or a
/// login shell.
pub fn remote_command(checkout: &str, argv: &[String]) -> String {
    let dir = shell_quote(checkout);
    let prefix = format!("mkdir -p {dir} && cd {dir} && ");
    if argv.is_empty() {
        format!("{prefix}exec \"${{SHELL:-/bin/bash}}\" -l")
    } else {
        let words: Vec<String> = argv.iter().map(|word| shell_quote(word)).collect();
        format!("{prefix}{}", words.join(" "))
    }
}

/// The quoted words of `argv`, as the remote shell runs them.
fn remote_words(argv: &[String]) -> String {
    argv.iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run `argv` with its stdout and stderr framed into `stream`.
fn exec_framed(target: &SshTarget, line: &str, stream: &mut Stream) -> Result<u8, AppError> {
    let mut child = target
        .ssh()
        .arg("-T")
        .arg(&target.alias)
        .arg(line)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::io("cannot run ssh", error))?;
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let mut pumps = Vec::new();
    for (is_out, pipe) in [
        (
            true,
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ),
        (
            false,
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ),
    ] {
        let Some(mut pipe) = pipe else { continue };
        let tx = tx.clone();
        pumps.push(std::thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            while let Ok(read) = pipe.read(&mut buffer) {
                if read == 0 || tx.send((is_out, buffer[..read].to_vec())).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    for (is_out, bytes) in rx {
        if is_out {
            stream.stdout(&bytes);
        } else {
            stream.stderr(&bytes);
        }
    }
    for pump in pumps {
        let _ = pump.join();
    }
    let status = child
        .wait()
        .map_err(|error| AppError::io("cannot run ssh", error))?;
    let code = match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => 255,
    };
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(255))
}

fn exec(target: &SshTarget, line: &str, shell: bool) -> Result<u8, AppError> {
    let interactive = shell && std::io::stdin().is_terminal();
    let mut command = target.ssh();
    command
        .arg(if interactive { "-t" } else { "-T" })
        .arg(&target.alias)
        .arg(line);
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
            remote_command("/home/user/app", &["uname".into(), "-a".into()]),
            "mkdir -p /home/user/app && cd /home/user/app && uname -a"
        );
        assert_eq!(
            remote_command(
                "/home/user/app",
                &["sh".into(), "-c".into(), "echo $HOME; exit 3".into()]
            ),
            "mkdir -p /home/user/app && cd /home/user/app && sh -c 'echo $HOME; exit 3'"
        );
        assert!(remote_command("/home/user/app", &[]).ends_with("exec \"${SHELL:-/bin/bash}\" -l"));
    }
}
