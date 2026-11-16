//! Credentials for `boatparty shell`: a `hold` invocation of the runner that
//! serves the GitHub token for as long as this process keeps its stdin open,
//! and the `shell` invocation that joins it.
//!
//! `shell` runs a shell or a command whose stdin (and, for a shell, terminal)
//! belongs to the user, so the credentials cannot travel at the head of its
//! stdin the way they do for `run` and `pipe`. The hold carries them
//! instead: the token reaches the sandbox only on the hold's stdin, and the
//! shell invocation names the hold's socket. The Git identity, which is not
//! secret, travels as arguments. When this process ends, SIGKILL included,
//! the hold's stdin closes and the runner stops the token server.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::credentials::RunCredentials;
use crate::error::AppError;
use crate::runner::{EXIT_MISSING, MISSING_MARKER, NO_SPEC, RunnerHost};

/// How long the runner gets to report that the hold is ready.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// A running hold. Dropping it ends the hold.
pub struct Hold {
    child: Child,
    stdin: Option<ChildStdin>,
    /// The token server's socket on the sandbox, when a token is served.
    pub socket: Option<String>,
    /// The runner (or the project's spec) was uploaded first.
    pub installed: bool,
}

impl Hold {
    /// The arguments of the `shell` invocation that joins this hold:
    /// `shell <project> <spec> <socket|-> <name> <email> [words...]`.
    pub fn board_args(
        &self,
        project: &str,
        spec: Option<String>,
        credentials: &RunCredentials,
        words: &[String],
    ) -> Vec<String> {
        board_args(project, spec, self.socket.as_deref(), credentials, words)
    }
}

/// The arguments of a `shell` invocation.
pub fn board_args(
    project: &str,
    spec: Option<String>,
    socket: Option<&str>,
    credentials: &RunCredentials,
    words: &[String],
) -> Vec<String> {
    let mut args = vec![
        "shell".to_string(),
        project.to_string(),
        spec.unwrap_or_else(|| NO_SPEC.to_string()),
        socket.unwrap_or("-").to_string(),
        credentials.git_name.clone().unwrap_or_default(),
        credentials.git_email.clone().unwrap_or_default(),
    ];
    args.extend(words.iter().cloned());
    args
}

impl Drop for Hold {
    fn drop(&mut self) {
        drop(self.stdin.take());
        // The runner stops the server and exits at end of file; give it a
        // moment, then cut the connection.
        for _ in 0..30 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a hold for `credentials` on `host`, installing the runner and the
/// project's spec first when they are missing.
pub fn hold(
    host: &dyn RunnerHost,
    project: &str,
    credentials: &RunCredentials,
) -> Result<Hold, AppError> {
    let mut installed = false;
    loop {
        match attempt(host, project, credentials)? {
            Attempt::Ready(mut hold) => {
                hold.installed = installed;
                return Ok(hold);
            }
            Attempt::Missing if !installed => {
                host.install()?;
                installed = true;
            }
            Attempt::Missing => {
                return Err(AppError::new(
                    "run_failed",
                    "the runner or the project's spec is still missing on the sandbox after uploading them",
                ));
            }
            Attempt::Failed(status, stderr) => {
                return Err(AppError::new(
                    if status == Some(255) {
                        "connection"
                    } else {
                        "run_failed"
                    },
                    format!(
                        "could not prepare the session's credentials (exit {}){}",
                        status.map_or_else(|| "by signal".to_string(), |code| code.to_string()),
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!(": {stderr}")
                        }
                    ),
                ));
            }
        }
    }
}

enum Attempt {
    Ready(Hold),
    Missing,
    Failed(Option<i32>, String),
}

fn attempt(
    host: &dyn RunnerHost,
    project: &str,
    credentials: &RunCredentials,
) -> Result<Attempt, AppError> {
    let spec = host.spec_hash().unwrap_or_else(|| NO_SPEC.to_string());
    let mut child = host
        .command(&["hold".to_string(), project.to_string(), spec])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::io("cannot run ssh", error))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // The token stays in this process's memory and the pipe only.
    let written = stdin
        .write_all(&credentials.header())
        .and_then(|()| stdin.flush());
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let read = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send(read);
        // Keep the pipe drained until the hold ends.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let line = rx.recv_timeout(READY_TIMEOUT).ok().and_then(Result::ok);
    if let (Ok(()), Some(line)) = (&written, &line)
        && let Some(socket) = line.trim_end().strip_prefix("ready ")
    {
        // Drain stderr in the background so the runner never blocks on it.
        if let Some(mut stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            });
        }
        return Ok(Attempt::Ready(Hold {
            child,
            stdin: Some(stdin),
            socket: (socket != "-" && !socket.is_empty()).then(|| socket.to_string()),
            installed: false,
        }));
    }
    drop(stdin);
    if line.is_none() {
        // No answer in time: cut the connection.
        let _ = child.kill();
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child
        .wait()
        .map_err(|error| AppError::io("cannot run ssh", error))?
        .code();
    if status == Some(EXIT_MISSING) && stderr.contains(MISSING_MARKER) {
        return Ok(Attempt::Missing);
    }
    Ok(Attempt::Failed(status, stderr.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::seed::Secret;

    #[test]
    fn board_arguments_carry_the_identity_and_the_socket_never_the_token() {
        let credentials = RunCredentials {
            git_name: Some("Ada Lovelace".into()),
            git_email: None,
            github_token: Some(Secret::new("gho_secret")),
        };
        let args = board_args(
            "app",
            Some("abc".into()),
            Some("/dev/shm/bpc-1/s"),
            &credentials,
            &["echo".into(), "hi".into()],
        );
        assert_eq!(
            args,
            [
                "shell",
                "app",
                "abc",
                "/dev/shm/bpc-1/s",
                "Ada Lovelace",
                "",
                "echo",
                "hi"
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("gho_secret")));
        let bare = board_args("app", None, None, &RunCredentials::none(), &[]);
        assert_eq!(bare, ["shell", "app", "-", "-", "", ""]);
    }
}
