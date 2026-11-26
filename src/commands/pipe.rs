//! `boatparty pipe`: one command in the sandbox's checkout, fed this
//! process's standard input, without syncing first.
//!
//! The input is for things that must not appear in a command line or a log
//! (signed URLs, tokens): it travels only on the runner's stdin, and nothing
//! boatparty writes (runs.jsonl, state.json, its own messages) holds it.
//! The command gets the project's credentials and `[run.env]` like `run`
//! and holds a `session` lease. When this process dies, SIGKILL included,
//! the sandbox stops the command within seconds, even if the input it was
//! given is still open: the input travels framed, so its end and the end of
//! the connection are told apart (see the runner's "Piped input").

use std::io::Read;
use std::time::Instant;

use serde_json::{Value, json};

use super::session::target_session;
use crate::Context;
use crate::cli::PipeArgs;
use crate::error::AppError;
use crate::runner::{RunnerSpec, SshRunner};
use crate::runs;
use crate::stream::Stream;
use crate::sync::{self, RunCredentials, RunEnd, TokenSource};
use crate::system;
use crate::transport::ssh::shell_quote;

pub fn run(ctx: &Context, args: &PipeArgs) -> Result<u8, AppError> {
    let line = command_line(args);
    let mut stream = Stream::new(ctx.out.is_json());
    let began = Instant::now();
    let at = system::now_ms();
    let mut record = json!({});
    let status = match execute(ctx, args.gate, &line, &mut stream, &mut record) {
        Ok(status) => status,
        Err(error) => {
            stream.error(&error);
            record["exit_code"] = Value::Null;
            record["error"] = json!(error.code());
            error.exit_code()
        }
    };
    record["at"] = json!(runs::rfc3339(at));
    record["kind"] = json!("pipe");
    record["command"] = json!(line);
    record["seconds"] = json!(round(began.elapsed().as_secs_f64()));
    if let Err(error) = runs::append(&ctx.paths, &record) {
        stream.note(format_args!("warning: could not record the run: {error}"));
    }
    Ok(status)
}

/// The shell line: `--sh` as given, or every word after `--` quoted.
pub fn command_line(args: &PipeArgs) -> String {
    match &args.sh {
        Some(line) => line.clone(),
        None => args
            .command
            .iter()
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn execute(
    ctx: &Context,
    gate: bool,
    line: &str,
    stream: &mut Stream,
    record: &mut Value,
) -> Result<u8, AppError> {
    let began = Instant::now();
    let ended = target_session(ctx, line, gate, |held| {
        let config = held.config;
        let token_source = config.run_token_source();
        // Read only now that the command is about to run; held in memory.
        let credentials =
            RunCredentials::gather(&held.workspace.root, config.git_identity(), &token_source);
        if token_source != TokenSource::None && credentials.github_token.is_none() {
            stream.note(
                "[run] github_token is on but no GitHub token was found (set \
                 BOATPARTY_GITHUB_TOKEN or run `gh auth login`); the command runs without one",
            );
        }
        let host = SshRunner {
            target: held.target.clone(),
            upload: None,
            spec: Some(RunnerSpec::new(config.config())),
            dedicated: None,
        };
        let id = held.member.id.clone().unwrap_or_default();
        record["project"] = json!(held.project);
        record["workspace"] = json!(held.workspace.key());
        record["member"] = json!(held.member.name);
        record["sandbox"] = json!(id);
        record["git_identity"] = json!(credentials.has_identity());
        record["github_token"] = json!(credentials.github_token.is_some());
        let mut input = || Box::new(std::io::stdin()) as Box<dyn Read + Send>;
        let outcome = sync::pipe_command(
            &host,
            held.project,
            line,
            &credentials,
            &mut input,
            stream,
            held.interrupts,
        );
        drop(credentials);
        Ok((outcome?, held.member.name.clone(), id))
    })?;
    let (outcome, member, id) = ended.value?;
    let cancelled = ended.interrupted;
    let status = match outcome.end {
        RunEnd::Done { exit_code, .. } => Some(exit_code),
        RunEnd::Lost { .. } if cancelled.is_some() => None,
        RunEnd::Lost { status } => return Err(lost(status, &member)),
    };
    let exit = cancelled
        .unwrap_or_else(|| u8::try_from(status.unwrap_or(125).clamp(0, 255)).unwrap_or(255));
    let seconds = round(began.elapsed().as_secs_f64());
    record["exit_code"] = json!(status);
    record["cancelled"] = json!(cancelled.is_some());
    record["input_bytes"] = json!(outcome.input_bytes);
    if stream.is_json() {
        stream.result(json!({
            "ok": true,
            "kind": "pipe",
            "exit_code": exit,
            "command_exit_code": status,
            "cancelled": cancelled.is_some(),
            "project": record["project"],
            "workspace": record["workspace"],
            "member": { "name": member, "id": id },
            "command": line,
            "seconds": seconds,
            "input_bytes": outcome.input_bytes,
            "runner_installed": outcome.installed,
            "git_identity": record["git_identity"],
            "github_token": record["github_token"],
        }));
    } else {
        let what = match status {
            Some(code) if cancelled.is_none() => format!("exit {code}"),
            Some(code) => format!("cancelled (command exit {code})"),
            None => "cancelled".to_string(),
        };
        stream.note(format_args!(
            "{member} ({id}) pipe {what} in {seconds:.1}s ({} input bytes)",
            outcome.input_bytes
        ));
    }
    Ok(exit)
}

fn lost(status: Option<i32>, member: &str) -> AppError {
    match status {
        Some(255) => AppError::new(
            "connection",
            format!(
                "the session on {member} ended without a result (ssh exit 255); the sandbox may have stopped, so run the command again"
            ),
        ),
        Some(code) => AppError::new(
            "run_failed",
            format!("the runner on {member} ended without a result (exit {code})"),
        ),
        None => AppError::new(
            "run_failed",
            format!("the connection to {member} was cut before a result arrived"),
        ),
    }
}

fn round(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_words_are_quoted_and_a_shell_line_is_passed_as_is() {
        let argv = PipeArgs {
            gate: false,
            sh: None,
            command: vec!["sh".into(), "-c".into(), "cat > f; wc -c f".into()],
        };
        assert_eq!(command_line(&argv), "sh -c 'cat > f; wc -c f'");
        let line = PipeArgs {
            gate: false,
            sh: Some("tee a | wc -l".into()),
            command: Vec::new(),
        };
        assert_eq!(command_line(&line), "tee a | wc -l");
    }
}
