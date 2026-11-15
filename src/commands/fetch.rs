//! `boatparty fetch`: copy a file or directory from the sandbox to this
//! machine, without syncing, under a `session` lease.
//!
//! The sandbox packs the path into a gzip-compressed tar under
//! `~/.boatparty/out/`. An archive above the signed-URL threshold comes
//! back through the provider's signed download URL (several times faster
//! than SSH); a smaller one, or one whose signed download fails, comes back
//! over SSH. A local `tar` unpacks it in a staging directory next to the
//! destination, and the result is renamed into place, so an interrupted
//! fetch never leaves a half-written file at the destination.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::json;

use super::session::with_session;
use crate::Context;
use crate::cli::FetchArgs;
use crate::error::AppError;
use crate::pool::remote_checkout;
use crate::provider::Provider;
use crate::runner::REMOTE_STATE_DIR;
use crate::system;
use crate::transport::ssh::{SshTarget, shell_quote};

/// Printed by the remote side when the path does not exist.
const MISSING_MARKER: &str = "boatparty-fetch-missing";

pub fn run(ctx: &Context, args: &FetchArgs) -> Result<u8, AppError> {
    let started = Instant::now();
    if args.path.trim().is_empty() {
        return Err(AppError::new("fetch_failed", "the path to fetch is empty"));
    }
    let dest = args.dest.clone().unwrap_or_else(|| PathBuf::from("."));
    let label = format!("fetch {}", args.path);
    let ended = with_session(ctx, &label, |held| {
        let remote = remote_path(held.project, &args.path);
        let signed = held.member.id.clone().and_then(|sandbox_id| {
            Some(Signed {
                provider: super::open_provider(ctx).ok()?,
                sandbox_id,
                threshold: crate::runner::upload_threshold(),
            })
        });
        let fetched = transfer(held.target, signed.as_ref(), &remote, &dest)?;
        Ok((remote, fetched, held.member.clone()))
    })?;
    if let Some(status) = ended.interrupted {
        return Ok(status);
    }
    let (remote, fetched, member) = ended.value?;
    let seconds = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let id = member.id.clone().unwrap_or_default();
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "member": { "name": member.name, "id": id },
            "remote_path": remote,
            "local_path": fetched.local.display().to_string(),
            "kind": fetched.kind,
            "files": fetched.files,
            "bytes": fetched.bytes,
            "archive_bytes": fetched.archive_bytes,
            "route": fetched.route,
            "download_fallback": fetched.fallback,
            "seconds": seconds,
        }));
    } else {
        if let Some(reason) = &fetched.fallback {
            ctx.out.note(format_args!(
                "the signed download failed ({reason}); fetched over SSH instead"
            ));
        }
        ctx.out.text(format_args!(
            "fetched {remote} from {} ({id}) to {}: {} {}, {} bytes, {seconds:.1}s{}",
            member.name,
            fetched.local.display(),
            fetched.files,
            if fetched.files == 1 { "file" } else { "files" },
            fetched.bytes,
            if fetched.route == "signed_url" {
                " by signed download"
            } else {
                ""
            },
        ));
    }
    Ok(0)
}

/// The absolute path on the sandbox: relative paths are under the checkout.
pub fn remote_path(project: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{path}", remote_checkout(project))
    }
}

/// The remote side: pack `path` into `archive` as a gzip-compressed tar
/// holding one entry named after it, and print the archive's size.
/// Directories are normalized first, so `dir/` and `.` name the directory
/// itself. Symbolic links are followed.
pub fn pack_script(path: &str, archive: &str) -> String {
    format!(
        "p={}; a={}; if [ ! -e \"$p\" ]; then echo {MISSING_MARKER} >&2; exit 4; fi; \
         if [ -d \"$p\" ]; then p=$(cd \"$p\" && pwd -L) || exit 4; fi; \
         if [ \"$p\" = / ]; then echo 'boatparty: refusing to fetch /' >&2; exit 4; fi; \
         mkdir -p \"$(dirname \"$a\")\" && cd \"$(dirname \"$p\")\" && \
         tar -czhf \"$a\" -- \"$(basename \"$p\")\" && wc -c < \"$a\"",
        shell_quote(path),
        shell_quote(archive)
    )
}

/// The provider's signed download, when it can be used.
struct Signed {
    provider: Box<dyn Provider>,
    sandbox_id: String,
    threshold: u64,
}

/// What a fetch brought back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub local: PathBuf,
    /// `"file"` or `"directory"`.
    pub kind: &'static str,
    pub files: u64,
    pub bytes: u64,
    /// The compressed archive's size.
    pub archive_bytes: u64,
    /// `"ssh"` or `"signed_url"`.
    pub route: &'static str,
    /// Why the signed download failed, when SSH was used instead.
    pub fallback: Option<String>,
}

fn transfer(
    target: &SshTarget,
    signed: Option<&Signed>,
    remote: &str,
    dest: &Path,
) -> Result<Fetched, AppError> {
    let into_dir = dest.is_dir();
    let parent = if into_dir {
        dest.to_path_buf()
    } else {
        match dest.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        }
    };
    if !parent.is_dir() {
        return Err(AppError::new(
            "fetch_failed",
            format!("{} is not a directory", parent.display()),
        ));
    }
    let staging = Staging::create(&parent)?;
    let archive = format!(
        "{REMOTE_STATE_DIR}/out/fetch-{}.tgz",
        system::random_hex(8)?
    );
    let packed = ssh_output(target, &pack_script(remote, &archive))?;
    let stderr = String::from_utf8_lossy(&packed.stderr);
    if stderr.contains(MISSING_MARKER) {
        return Err(AppError::new(
            "fetch_failed",
            format!("no such file or directory on the sandbox: {remote}"),
        ));
    }
    if !packed.status.success() {
        return Err(ssh_failure(
            &packed,
            &format!("packing {remote} on the sandbox"),
        ));
    }
    let archive_bytes: u64 = String::from_utf8_lossy(&packed.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    let local_archive = staging.path.join("archive.tgz");
    let mut route = "ssh";
    let mut fallback = None;
    if let Some(signed) = signed.filter(|signed| archive_bytes > signed.threshold) {
        match signed
            .provider
            .download_file(&signed.sandbox_id, &archive, &local_archive)
        {
            Ok(_) => route = "signed_url",
            Err(error) => fallback = Some(AppError::from(error).message().to_string()),
        }
    }
    let quoted = shell_quote(&archive);
    let read = if route == "ssh" {
        // Read and remove in one round trip.
        let script = format!("cat {quoted}; rc=$?; rm -f {quoted}; exit $rc");
        let file = fs::File::create(&local_archive).map_err(|error| {
            AppError::io(format!("cannot write {}", local_archive.display()), error)
        })?;
        let output = target
            .ssh()
            .arg("-T")
            .arg(&target.alias)
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| AppError::io("cannot run ssh", error))?;
        if !output.status.success() {
            return Err(ssh_failure(&output, &format!("reading {remote}")));
        }
        Ok(())
    } else {
        ssh_output(target, &format!("rm -f {quoted}")).map(|_| ())
    };
    read?;
    let unpacked = staging.path.join("entry");
    fs::create_dir(&unpacked)
        .map_err(|error| AppError::io(format!("cannot create {}", unpacked.display()), error))?;
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&local_archive)
        .arg("-C")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AppError::io("cannot run tar", error))?;
    if !tar.status.success() {
        return Err(AppError::new(
            "fetch_failed",
            format!(
                "unpacking {remote} failed: {}",
                String::from_utf8_lossy(&tar.stderr).trim()
            ),
        ));
    }
    let entry = single_entry(&unpacked)?;
    let name = entry
        .file_name()
        .map(PathBuf::from)
        .ok_or_else(|| AppError::new("fetch_failed", "the sandbox sent an unnamed entry"))?;
    let local = if into_dir {
        dest.join(&name)
    } else {
        dest.to_path_buf()
    };
    let is_dir = fs::symlink_metadata(&entry)
        .map(|meta| meta.is_dir())
        .unwrap_or(false);
    if let Ok(existing) = fs::symlink_metadata(&local)
        && (existing.is_dir() || is_dir)
    {
        return Err(AppError::new(
            "fetch_failed",
            format!("{} already exists", local.display()),
        )
        .with_hint("remove it or choose another destination"));
    }
    let (files, bytes) = measure(&entry);
    fs::rename(&entry, &local).map_err(|error| {
        AppError::io(
            format!("cannot move the fetched copy to {}", local.display()),
            error,
        )
    })?;
    let local = fs::canonicalize(&local).unwrap_or(local);
    Ok(Fetched {
        local,
        kind: if is_dir { "directory" } else { "file" },
        files,
        bytes,
        archive_bytes,
        route,
        fallback,
    })
}

/// Run one remote command and collect its output.
fn ssh_output(target: &SshTarget, script: &str) -> Result<std::process::Output, AppError> {
    target
        .ssh()
        .arg("-T")
        .arg(&target.alias)
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| AppError::io("cannot run ssh", error))
}

fn ssh_failure(output: &std::process::Output, what: &str) -> AppError {
    let code = if output.status.code() == Some(255) {
        "connection"
    } else {
        "fetch_failed"
    };
    AppError::new(
        code,
        format!(
            "{what} failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    )
}

/// The one entry the archive held.
fn single_entry(dir: &Path) -> Result<PathBuf, AppError> {
    let entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| AppError::io(format!("cannot read {}", dir.display()), error))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    match entries.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(AppError::new(
            "fetch_failed",
            format!("the sandbox sent {} entries instead of one", entries.len()),
        )),
    }
}

/// Regular files and their total size under `path`.
fn measure(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (1, meta.len());
    }
    let mut totals = (0, 0);
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let (files, bytes) = measure(&entry.path());
            totals.0 += files;
            totals.1 += bytes;
        }
    }
    totals
}

/// A hidden directory next to the destination, removed on drop.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(parent: &Path) -> Result<Self, AppError> {
        let path = parent.join(format!(".boatparty-fetch-{}", system::random_hex(6)?));
        fs::create_dir(&path)
            .map_err(|error| AppError::io(format!("cannot create {}", path.display()), error))?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_resolve_under_the_checkout() {
        assert_eq!(
            remote_path("app", "target/release/app"),
            "/home/user/app/target/release/app"
        );
        assert_eq!(remote_path("app", "/tmp/out.bin"), "/tmp/out.bin");
    }

    #[test]
    fn the_remote_script_quotes_the_path_and_reports_a_missing_one() {
        let script = pack_script("/home/user/app/my file", "/home/user/.boatparty/out/f.tgz");
        assert!(
            script.starts_with("p='/home/user/app/my file'; a=/home/user/.boatparty/out/f.tgz; ")
        );
        assert!(script.contains(MISSING_MARKER));
        assert!(script.contains("tar -czhf \"$a\""));
    }
}
