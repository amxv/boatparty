//! `boatparty doctor`: is this machine ready, and does the key allow
//! everything boatparty does? Every failed check prints the command that
//! fixes it. The key itself is never shown.
//!
//! It also matches every project's records with the provider (as `pool
//! --refresh` does) and then reports leftovers: SSH configs and pinned host
//! keys of sandboxes no record names, and control sockets no master answers
//! on. Those are warnings, with the command that removes them.

use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Context;
use crate::account::{self, KeySource, credentials};
use crate::error::AppError;
use crate::output::error_document;
use crate::provider::{self, CredentialCheck, CredentialHelp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// Worth cleaning up; not a failure.
    Warn,
    Fail,
    Skipped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: Status,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Skipped,
            detail: detail.into(),
            fix: None,
        }
    }
}

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let help = provider::credential_help();
    let mut checks = Vec::new();

    for (tool, fix) in [
        ("ssh", install_hint("openssh-client")),
        ("ssh-keygen", install_hint("openssh-client")),
        ("git", install_hint("git")),
    ] {
        checks.push(match find_on_path(tool) {
            Some(path) => Check::ok(tool, path.display().to_string()),
            None => Check::fail(tool, format!("{tool} is not on PATH"), fix),
        });
    }

    let account = match account::load_or_create(&ctx.paths) {
        Ok(loaded) => {
            let created = if loaded.created {
                " (created with defaults)"
            } else {
                ""
            };
            checks.push(Check::ok(
                "account",
                format!("{}{created}", loaded.path.display()),
            ));
            Some(loaded)
        }
        Err(error) => {
            checks.push(Check::fail(
                "account",
                error.message(),
                format!(
                    "edit {} to fix it, or move it aside so boatparty recreates it",
                    ctx.paths.account_file().display()
                ),
            ));
            None
        }
    };

    let key = match credentials::load(&ctx.paths) {
        Ok(Some((key, source))) => {
            checks.push(credential_check(&source));
            Some(key)
        }
        Ok(None) => {
            checks.push(Check::fail(
                "credentials",
                format!(
                    "no API key: {} does not exist and {} is not set",
                    ctx.paths.credentials_file().display(),
                    account::KEY_ENV
                ),
                help.create_and_login_command.clone(),
            ));
            None
        }
        Err(error) => {
            checks.push(Check::fail(
                "credentials",
                error.message(),
                help.create_and_login_command.clone(),
            ));
            None
        }
    };

    match (account, key) {
        (Some(account), Some(key)) => {
            match provider::connect(account.config.provider.api_url.as_deref(), key) {
                Ok(provider) => {
                    let key_checks = key_checks(provider.check_credential(), &help);
                    let usable = key_checks
                        .first()
                        .is_some_and(|check| check.status == Status::Ok);
                    checks.extend(key_checks);
                    checks.push(if usable {
                        pool_check(ctx, provider.as_ref(), &account.config)
                    } else {
                        Check::skipped("pools", "needs a valid key")
                    });
                }
                Err(error) => checks.push(Check::fail(
                    "key",
                    error.message(),
                    format!("fix provider.api_url in {}", account.path.display()),
                )),
            }
        }
        _ => {
            checks.push(Check::skipped(
                "key",
                "needs a readable account file and a key",
            ));
            checks.push(Check::skipped("key_scope", "needs a valid key"));
            checks.push(Check::skipped("pools", "needs a valid key"));
        }
    }

    // After the pools check, which may have dropped records.
    checks.push(ssh_files_check(ctx));
    checks.push(control_sockets_check(ctx));

    let failed = checks
        .iter()
        .filter(|check| check.status == Status::Fail)
        .count();
    if ctx.out.is_json() {
        ctx.out.document(&document(&checks, failed));
    } else {
        ctx.out.text(render(&checks).trim_end());
    }
    if failed == 0 {
        Ok(0)
    } else {
        if !ctx.out.is_json() {
            ctx.out.note(format_args!(
                "{failed} check{} failed",
                if failed == 1 { "" } else { "s" }
            ));
        }
        Ok(crate::error::EXIT_TOOL_FAILURE)
    }
}

/// Reconcile every project's records with the provider's labelled
/// sandboxes (as `boatparty pool --refresh` does).
fn pool_check(
    ctx: &Context,
    provider: &dyn provider::Provider,
    config: &account::config::AccountConfig,
) -> Check {
    let state = match crate::state::Store::new(&ctx.paths).load() {
        Ok(state) => state,
        Err(error) => {
            return Check::fail(
                "pools",
                error.message(),
                "move the state file aside; `boatparty pool --refresh` rebuilds the records",
            );
        }
    };
    let mut names: std::collections::BTreeSet<String> = config.projects.keys().cloned().collect();
    names.extend(state.projects.keys().cloned());
    if names.is_empty() {
        return Check::ok("pools", "no projects yet");
    }
    let (mut adopted, mut dropped, mut expired, mut freed) = (0, 0, 0, 0);
    let mut errors = Vec::new();
    for name in &names {
        match crate::pool::reconcile::reconcile(&ctx.paths, provider, ctx.out, name) {
            Ok((changes, _)) => {
                adopted += changes.adopted.len();
                dropped += changes.dropped.len();
                expired += changes.expired.len();
                freed += changes.freed.len();
                errors.extend(changes.errors);
            }
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    if !errors.is_empty() {
        return Check::fail(
            "pools",
            errors.join("; "),
            "check the network connection and run `boatparty pool --refresh`",
        );
    }
    Check::ok(
        "pools",
        format!(
            "{} project{} matched with the provider: {adopted} adopted, {dropped} dropped as gone, \
             {expired} deleted with expired data, {freed} freed from missing workspaces",
            names.len(),
            if names.len() == 1 { "" } else { "s" }
        ),
    )
}

/// SSH configs and pinned host keys (`<id>.conf`, `<id>.known_hosts`) of
/// sandboxes that no record in state.json names.
fn ssh_files_check(ctx: &Context) -> Check {
    let state = match crate::state::Store::new(&ctx.paths).load() {
        Ok(state) => state,
        Err(error) => return Check::skipped("ssh_configs", error.message()),
    };
    let known: std::collections::BTreeSet<String> = state
        .projects
        .values()
        .flat_map(|project| project.members.iter())
        .filter_map(|member| member.id.clone())
        .collect();
    let dir = ctx.paths.ssh_state_dir();
    let mut total = 0;
    let mut stale = Vec::new();
    for path in files_in(&dir) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = name
            .strip_suffix(".conf")
            .or_else(|| name.strip_suffix(".known_hosts"))
        else {
            continue;
        };
        total += 1;
        if !known.contains(id) {
            stale.push(path);
        }
    }
    if stale.is_empty() {
        return Check::ok(
            "ssh_configs",
            format!(
                "{total} file{} in {}, none stale",
                plural(total),
                dir.display()
            ),
        );
    }
    Check::warn(
        "ssh_configs",
        format!(
            "{} stale file{} for sandboxes no record names: {}",
            stale.len(),
            plural(stale.len()),
            names(&stale)
        ),
        remove_command(&stale),
    )
}

/// Control sockets no SSH master answers on: what a master that died
/// without cleaning up leaves behind.
#[cfg(windows)]
fn control_sockets_check(_: &Context) -> Check {
    Check::skipped(
        "control_sockets",
        "Windows OpenSSH opens a connection per invocation",
    )
}

#[cfg(unix)]
fn control_sockets_check(ctx: &Context) -> Check {
    use std::os::unix::fs::FileTypeExt;
    let dir = ctx.paths.control_dir();
    let mut total = 0;
    let mut stale = Vec::new();
    for path in files_in(&dir) {
        let is_socket = fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_socket())
            .unwrap_or(false);
        if !is_socket {
            continue;
        }
        total += 1;
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            stale.push(path);
        }
    }
    if stale.is_empty() {
        return Check::ok(
            "control_sockets",
            format!(
                "{total} socket{} in {}, none stale",
                plural(total),
                dir.display()
            ),
        );
    }
    Check::warn(
        "control_sockets",
        format!(
            "{} stale socket{} no SSH master answers on: {}",
            stale.len(),
            plural(stale.len()),
            names(&stale)
        ),
        remove_command(&stale),
    )
}

/// The entries of `dir`, sorted; none when it does not exist.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

fn names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

fn remove_command(paths: &[PathBuf]) -> String {
    let quoted: Vec<String> = paths.iter().map(|path| shell_quote(path)).collect();
    format!("rm -f {}", quoted.join(" "))
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn credential_check(source: &KeySource) -> Check {
    match source {
        KeySource::Environment => {
            Check::ok("credentials", format!("API key from {}", source.describe()))
        }
        #[cfg(windows)]
        KeySource::File(path) => match crate::system::windows::private_acl(path) {
            Ok(true) => Check::ok(
                "credentials",
                format!("API key in {} (private ACL)", path.display()),
            ),
            _ => Check::fail(
                "credentials",
                format!("cannot verify private access to {}", path.display()),
                "store the key again with boatparty login --key-stdin",
            ),
        },
        #[cfg(unix)]
        KeySource::File(path) => match credentials::file_mode(path) {
            Some(mode) if mode & 0o077 != 0 => Check::fail(
                "credentials",
                format!("{} is readable by others (mode {mode:o})", path.display()),
                format!("chmod 600 {}", shell_quote(path)),
            ),
            _ => Check::ok(
                "credentials",
                format!("API key in {} (mode 0600)", path.display()),
            ),
        },
    }
}

fn key_checks(
    result: Result<CredentialCheck, provider::ProviderError>,
    help: &CredentialHelp,
) -> Vec<Check> {
    let replace = help.create_and_login_command.clone();
    match result {
        Ok(CredentialCheck::Complete) => vec![
            Check::ok("key", "the provider accepts the API key"),
            Check::ok(
                "key_scope",
                format!(
                    "allows all {} permissions boatparty uses",
                    help.permissions.len()
                ),
            ),
        ],
        Ok(CredentialCheck::MissingPermissions(missing)) => vec![
            Check::ok("key", "the provider accepts the API key"),
            Check::fail(
                "key_scope",
                format!("the key lacks: {}", missing.join(", ")),
                replace,
            ),
        ],
        Ok(CredentialCheck::RestrictedResources) => vec![
            Check::ok("key", "the provider accepts the API key"),
            Check::fail(
                "key_scope",
                "the key is limited to particular sandboxes or environments; boatparty creates its own sandboxes",
                replace,
            ),
        ],
        Ok(CredentialCheck::Invalid) => vec![
            Check::fail("key", "the provider rejects the API key", replace),
            Check::skipped("key_scope", "needs a valid key"),
        ],
        Ok(CredentialCheck::Expired) => vec![
            Check::fail("key", "the API key has expired", replace),
            Check::skipped("key_scope", "needs a valid key"),
        ],
        Err(error) => vec![
            Check::fail(
                "key",
                format!("cannot check the key: {error}"),
                "check the network connection and run `boatparty doctor` again",
            ),
            Check::skipped("key_scope", "needs a reachable provider"),
        ],
    }
}

fn document(checks: &[Check], failed: usize) -> Value {
    let checks: Vec<Value> = checks
        .iter()
        .map(|check| {
            json!({
                "name": check.name,
                "status": check.status.as_str(),
                "detail": check.detail,
                "fix": check.fix,
            })
        })
        .collect();
    if failed == 0 {
        json!({ "ok": true, "checks": checks })
    } else {
        let mut document = error_document(&AppError::new(
            "doctor_failed",
            format!(
                "{failed} doctor check{} failed",
                if failed == 1 { "" } else { "s" }
            ),
        ));
        document["checks"] = Value::Array(checks);
        document
    }
}

fn render(checks: &[Check]) -> String {
    let mut text = String::new();
    for check in checks {
        let label = match check.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Skipped => "skip",
        };
        text.push_str(&format!("{label}  {:<12} {}\n", check.name, check.detail));
        if let Some(fix) = &check.fix {
            text.push_str(&format!("      {:<12} fix: {fix}\n", ""));
        }
    }
    text
}

/// The first executable named `name` on `PATH`. Nothing is run.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    #[cfg(windows)]
    let name = format!("{name}.exe");
    env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(Path::new(&name)))
        .find(|candidate| is_executable(candidate))
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn install_hint(debian_package: &str) -> String {
    if cfg!(windows) {
        match debian_package {
            "git" => "install Git for Windows and add git.exe to PATH".to_string(),
            _ => "install the Windows OpenSSH Client optional feature and add System32\\OpenSSH to PATH".to_string(),
        }
    } else if cfg!(target_os = "macos") {
        match debian_package {
            "git" => "xcode-select --install   (or: brew install git)".to_string(),
            _ => "OpenSSH ships with macOS; restore /usr/bin/ssh or run: brew install openssh"
                .to_string(),
        }
    } else {
        format!("sudo apt-get install {debian_package}   (or your distribution's package)")
    }
}

fn shell_quote(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_permissions_fail_with_the_create_command() {
        let help = provider::credential_help();
        let checks = key_checks(
            Ok(CredentialCheck::MissingPermissions(vec![
                "file.read".into(),
                "file.write".into(),
            ])),
            &help,
        );
        assert_eq!(checks[0].status, Status::Ok);
        assert_eq!(checks[1].status, Status::Fail);
        assert!(checks[1].detail.contains("file.read, file.write"));
        assert_eq!(
            checks[1].fix.as_deref(),
            Some(help.create_and_login_command.as_str())
        );
    }

    #[test]
    fn failed_report_has_the_error_shape() {
        let checks = vec![
            Check::ok("ssh", "/usr/bin/ssh"),
            Check::fail("git", "git is not on PATH", "install git"),
        ];
        let document = document(&checks, 1);
        assert_eq!(document["ok"], false);
        assert_eq!(document["error"]["code"], "doctor_failed");
        assert_eq!(document["checks"][1]["status"], "fail");
        assert_eq!(document["checks"][1]["fix"], "install git");
        assert_eq!(document["checks"][0]["fix"], Value::Null);
    }

    #[test]
    fn shell_quote_only_when_needed() {
        assert_eq!(shell_quote(Path::new("/a/b.toml")), "/a/b.toml");
        assert_eq!(shell_quote(Path::new("/a b/c")), "'/a b/c'");
    }
}
