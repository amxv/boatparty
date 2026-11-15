//! The runner: the shell script that is the remote half of boatparty.
//!
//! The script is embedded at build time and lives on each sandbox under its
//! content hash, so a sandbox always runs the matching version and no
//! command line carries the script text. An invocation whose version is
//! missing prints [`MISSING_MARKER`] on stderr and exits [`EXIT_MISSING`];
//! the client uploads the script and retries once.

pub mod trailer;

use std::process::{Command, Stdio};

use crate::error::AppError;
use crate::transport::ssh::{SshTarget, shell_quote};

pub use trailer::{Piece, Trailer, TrailerFilter};

/// The runner script.
pub const SCRIPT: &str = include_str!("runner.sh");

/// Exit status of an invocation whose runner version is not on the sandbox.
pub const EXIT_MISSING: i32 = 96;
/// Exit status that goes with the `needfull` trailer.
pub const EXIT_NEEDFULL: i32 = 97;
/// Printed on stderr with [`EXIT_MISSING`].
pub const MISSING_MARKER: &str = "boatparty-runner-missing";

/// The sandbox directory that holds the runner and its scratch files.
pub const REMOTE_STATE_DIR: &str = "/home/user/.boatparty";

/// The first 16 hex digits of the script's 64-bit FNV-1a hash. Only names
/// the version on the sandbox; it is not a security boundary.
pub fn script_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| content_hash(SCRIPT.as_bytes()))
}

/// 64-bit FNV-1a as 16 hex digits.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Where this version of the runner lives on a sandbox.
pub fn remote_script_path() -> String {
    format!("{REMOTE_STATE_DIR}/bin/runner-{}.sh", script_hash())
}

/// The remote shell command that runs the runner by path, or reports that
/// this version is missing.
pub fn remote_invocation(args: &[String]) -> String {
    let quoted: Vec<String> = args.iter().map(|arg| shell_quote(arg)).collect();
    format!(
        "f={}; if [ -f \"$f\" ]; then exec bash \"$f\" {}; else echo {MISSING_MARKER} >&2; exit {EXIT_MISSING}; fi",
        shell_quote(&remote_script_path()),
        quoted.join(" ")
    )
}

/// Where the runner runs: a sandbox over SSH, or (in tests) a local child.
pub trait RunnerHost {
    /// A command that runs the runner with these arguments. The caller sets
    /// up its standard streams.
    fn command(&self, args: &[String]) -> Command;

    /// Install this version of the runner, after an exit 96.
    fn install(&self) -> Result<(), AppError>;
}

/// The runner on a sandbox, reached through its SSH config.
pub struct SshRunner {
    pub target: SshTarget,
}

impl RunnerHost for SshRunner {
    fn command(&self, args: &[String]) -> Command {
        let mut command = self.target.ssh();
        command
            .arg("-T")
            .arg(&self.target.alias)
            .arg(remote_invocation(args));
        command
    }

    fn install(&self) -> Result<(), AppError> {
        let path = shell_quote(&remote_script_path());
        let remote = format!(
            "mkdir -p {dir}/bin && cat > {path}.$$.tmp && mv {path}.$$.tmp {path}",
            dir = shell_quote(REMOTE_STATE_DIR),
        );
        let mut child = self
            .target
            .ssh()
            .args(["-T", &self.target.alias, &remote])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| AppError::io("cannot run ssh", error))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let written = std::io::Write::write_all(&mut stdin, SCRIPT.as_bytes());
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|error| AppError::io("cannot run ssh", error))?;
        if !output.status.success() || written.is_err() {
            return Err(AppError::new(
                "ssh",
                format!(
                    "uploading the runner to {} failed (exit {}): {}",
                    self.target.alias,
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_is_named_by_its_content_hash() {
        assert_eq!(script_hash().len(), 16);
        assert_eq!(content_hash(b""), "cbf29ce484222325");
        assert_eq!(content_hash(b"a"), "af63dc4c8601ec8c");
        assert_eq!(
            remote_script_path(),
            format!("/home/user/.boatparty/bin/runner-{}.sh", script_hash())
        );
    }

    #[test]
    fn the_invocation_runs_by_path_and_reports_a_missing_version() {
        let line = remote_invocation(&["sync".into(), "app".into(), String::new()]);
        assert!(line.starts_with(&format!("f={}; ", remote_script_path())));
        assert!(line.contains("exec bash \"$f\" sync app ''"));
        assert!(line.contains("echo boatparty-runner-missing >&2; exit 96"));
        assert!(
            !line.contains("set -euo"),
            "the script text is never inlined"
        );
    }
}
