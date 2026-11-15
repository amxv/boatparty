//! The runner: the shell script that is the remote half of boatparty.
//!
//! The script is embedded at build time and lives on each sandbox under its
//! content hash, so a sandbox always runs the matching version and no
//! command line carries the script text. An invocation whose version is
//! missing prints [`MISSING_MARKER`] on stderr and exits [`EXIT_MISSING`];
//! the client uploads the script and retries once. The same holds for the
//! project's [`RunnerSpec`], which `sync` and `run` name.

pub mod spec;
pub mod trailer;

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::AppError;
use crate::provider::Provider;
use crate::transport::ssh::{SshTarget, shell_quote};

pub use spec::{NO_SPEC, RunnerSpec};
pub use trailer::{Piece, Trailer, TrailerFilter};

/// The runner script: one bash file, kept in the repository as focused parts
/// under `script/` and joined here in this order at build time. The parts are
/// not standalone scripts; only the joined text is uploaded and hashed.
pub const SCRIPT: &str = concat!(
    include_str!("script/core.sh"),
    include_str!("script/restore.sh"),
    include_str!("script/steps.sh"),
    include_str!("script/credentials.sh"),
    include_str!("script/sync.sh"),
    include_str!("script/serve.sh"),
    include_str!("script/pipe.sh"),
    include_str!("script/run.sh"),
);

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
/// this version is missing. The checkout is the runner's default,
/// `/home/user/<project>`.
pub fn remote_invocation(args: &[String]) -> String {
    remote_invocation_in(None, args)
}

/// [`remote_invocation`] for a project whose checkout is `checkout`
/// (`[sandbox] remote_path`), passed to the runner in its environment.
pub fn remote_invocation_in(checkout: Option<&str>, args: &[String]) -> String {
    let quoted: Vec<String> = args.iter().map(|arg| shell_quote(arg)).collect();
    let env = checkout
        .map(|dir| format!("BOATPARTY_RUNNER_REPO={} ", shell_quote(dir)))
        .unwrap_or_default();
    format!(
        "f={}; if [ -f \"$f\" ]; then {env}exec bash \"$f\" {}; else echo {MISSING_MARKER} >&2; exit {EXIT_MISSING}; fi",
        shell_quote(&remote_script_path()),
        quoted.join(" ")
    )
}

/// Where the runner runs: a sandbox over SSH, or (in tests) a local child.
pub trait RunnerHost {
    /// A command that runs the runner with these arguments. The caller sets
    /// up its standard streams.
    fn command(&self, args: &[String]) -> Command;

    /// Install this version of the runner, and the spec when there is one,
    /// after an exit 96.
    fn install(&self) -> Result<(), AppError>;

    /// The hash of the project's spec that `sync` and `run` name; `None`
    /// when this host applies no project settings.
    fn spec_hash(&self) -> Option<String> {
        None
    }

    /// The pack size above which [`RunnerHost::upload`] is used instead of
    /// the runner's stdin; `None` when this host has no other route.
    fn upload_threshold(&self) -> Option<u64> {
        None
    }

    /// Put `file` where the runner reads uploaded packs, as `<name>`
    /// ([`REMOTE_UPLOAD_DIR`] on a sandbox).
    fn upload(&self, _name: &str, _file: &Path) -> Result<(), AppError> {
        Err(AppError::new(
            "sync_failed",
            "this host has no upload route",
        ))
    }

    /// Copy `<name>` from where the runner leaves its results
    /// ([`REMOTE_OUT_DIR`] on a sandbox) into `file`, by the same route as
    /// [`RunnerHost::upload`].
    fn download(&self, _name: &str, _file: &Path) -> Result<(), AppError> {
        Err(AppError::new(
            "sync_failed",
            "this host has no download route",
        ))
    }
}

/// Packs (and `fetch` archives) larger than this go through the provider's
/// signed file URLs: SSH uploads at about 0.3 to 0.4 MB/s and a signed URL
/// at about 4.5 MB/s after a 0.35 s request for the URL (docs/boat-facts.md,
/// question 2), and downloads differ about as much, so the URL wins from a
/// few hundred KiB.
pub const DEFAULT_UPLOAD_THRESHOLD: u64 = 512 * 1024;
/// Overrides [`DEFAULT_UPLOAD_THRESHOLD`] (bytes) for uploads and downloads,
/// for tests and tuning.
pub const UPLOAD_THRESHOLD_ENV: &str = "BOATPARTY_SIGNED_URL_MIN_BYTES";

/// The sandbox directory the runner reads uploaded packs from.
pub const REMOTE_UPLOAD_DIR: &str = "/home/user/.boatparty/in";
/// The sandbox directory the runner leaves results (patches) in.
pub const REMOTE_OUT_DIR: &str = "/home/user/.boatparty/out";

/// The upload threshold in effect.
pub fn upload_threshold() -> u64 {
    std::env::var(UPLOAD_THRESHOLD_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_UPLOAD_THRESHOLD)
}

/// The runner on a sandbox, reached through its SSH config.
pub struct SshRunner {
    pub target: SshTarget,
    /// The provider's bulk upload for large packs, when available.
    pub upload: Option<SignedUpload>,
    /// The project's settings, named in `sync` and `run`.
    pub spec: Option<RunnerSpec>,
    /// Options (forwards) for a connection of its own that carries the
    /// `serve` invocation, and only it; `None` keeps every invocation on the
    /// shared master.
    pub dedicated: Option<Vec<String>>,
}

/// Large packs through the provider's signed upload route.
pub struct SignedUpload {
    pub provider: Box<dyn Provider>,
    pub sandbox_id: String,
    pub threshold: u64,
}

impl SshRunner {
    /// Reached over SSH only.
    pub fn ssh_only(target: SshTarget) -> Self {
        Self {
            target,
            upload: None,
            spec: None,
            dedicated: None,
        }
    }
}

impl RunnerHost for SshRunner {
    fn command(&self, args: &[String]) -> Command {
        let mut command = match (&self.dedicated, args.first().map(String::as_str)) {
            (Some(options), Some("serve")) => {
                let mut command = self.target.ssh_dedicated();
                command.args(options);
                command
            }
            _ => self.target.ssh(),
        };
        command
            .arg("-T")
            .arg(&self.target.alias)
            .arg(remote_invocation_in(
                self.spec.as_ref().and_then(RunnerSpec::checkout),
                args,
            ));
        command
    }

    fn upload_threshold(&self) -> Option<u64> {
        self.upload.as_ref().map(|upload| upload.threshold)
    }

    fn spec_hash(&self) -> Option<String> {
        self.spec.as_ref().map(|spec| spec.hash().to_string())
    }

    fn upload(&self, name: &str, file: &Path) -> Result<(), AppError> {
        let Some(upload) = &self.upload else {
            return Err(AppError::new("sync_failed", "no upload route"));
        };
        if name.contains('/') {
            return Err(AppError::new(
                "sync_failed",
                "upload names are plain file names",
            ));
        }
        upload
            .provider
            .upload_file(
                &upload.sandbox_id,
                &format!("{REMOTE_UPLOAD_DIR}/{name}"),
                file,
            )
            .map_err(AppError::from)
    }

    fn download(&self, name: &str, file: &Path) -> Result<(), AppError> {
        let Some(upload) = &self.upload else {
            return Err(AppError::new("sync_failed", "no download route"));
        };
        if name.contains('/') {
            return Err(AppError::new(
                "sync_failed",
                "download names are plain file names",
            ));
        }
        upload
            .provider
            .download_file(
                &upload.sandbox_id,
                &format!("{REMOTE_OUT_DIR}/{name}"),
                file,
            )
            .map(drop)
            .map_err(AppError::from)
    }

    fn install(&self) -> Result<(), AppError> {
        self.put(&remote_script_path(), SCRIPT.as_bytes(), "the runner")?;
        if let Some(spec) = &self.spec {
            self.put(
                &spec.remote_path(),
                spec.text().as_bytes(),
                "the project's spec",
            )?;
        }
        Ok(())
    }
}

impl SshRunner {
    /// Write a file on the sandbox atomically.
    fn put(&self, path: &str, bytes: &[u8], what: &str) -> Result<(), AppError> {
        let dir = Path::new(path)
            .parent()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| REMOTE_STATE_DIR.to_string());
        let path = shell_quote(path);
        let remote = format!(
            "mkdir -p {dir} && cat > {path}.$$.tmp && mv {path}.$$.tmp {path}",
            dir = shell_quote(&dir),
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
        let written = std::io::Write::write_all(&mut stdin, bytes);
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|error| AppError::io("cannot run ssh", error))?;
        if !output.status.success() || written.is_err() {
            return Err(AppError::new(
                "ssh",
                format!(
                    "uploading {what} to {} failed (exit {}): {}",
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
    fn the_parts_join_into_one_script() {
        let parts = [
            include_str!("script/core.sh"),
            include_str!("script/restore.sh"),
            include_str!("script/steps.sh"),
            include_str!("script/credentials.sh"),
            include_str!("script/sync.sh"),
            include_str!("script/serve.sh"),
            include_str!("script/pipe.sh"),
            include_str!("script/run.sh"),
        ];
        // Each part ends a line, so no line runs into the next part.
        for part in parts {
            assert!(part.ends_with('\n'));
        }
        assert!(SCRIPT.starts_with("#!/usr/bin/env bash\n"));
        assert_eq!(SCRIPT.matches("#!/usr/bin/env bash").count(), 1);
        assert_eq!(SCRIPT, parts.concat());
        assert!(SCRIPT.trim_end().ends_with("esac"));
    }

    #[test]
    fn the_invocation_runs_by_path_and_reports_a_missing_version() {
        let line = remote_invocation(&["sync".into(), "app".into(), String::new()]);
        assert!(line.starts_with(&format!("f={}; ", remote_script_path())));
        assert!(line.contains("exec bash \"$f\" sync app ''"));
        assert!(line.contains("echo boatparty-runner-missing >&2; exit 96"));
        let moved = remote_invocation_in(Some("/home/user/code/app"), &["sync".into()]);
        assert!(
            moved.contains("then BOATPARTY_RUNNER_REPO=/home/user/code/app exec bash \"$f\" sync;"),
            "{moved}"
        );
        assert!(
            !line.contains("set -euo"),
            "the script text is never inlined"
        );
    }
}
