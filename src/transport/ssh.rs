//! OpenSSH transport: the tool's own key, one generated config and one pinned
//! host key per sandbox, and a shared control master.
//!
//! The host alias in every config is the sandbox id, which is also the
//! `HostKeyAlias`, so a pinned key never depends on the address the sandbox
//! happens to have after a resume.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::SshAccess;

/// One sandbox's SSH files and alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// The `Host` alias: the sandbox id.
    pub alias: String,
    pub config: PathBuf,
    pub known_hosts: PathBuf,
}

impl SshTarget {
    pub fn for_sandbox(paths: &Paths, sandbox_id: &str) -> Self {
        Self {
            alias: sandbox_id.to_string(),
            config: paths.ssh_config_file(sandbox_id),
            known_hosts: paths.known_hosts_file(sandbox_id),
        }
    }

    /// `ssh` with this target's config that goes through the control master
    /// and never falls back to a connection of its own, before any options
    /// or the alias. When the master is gone, or dies because the sandbox
    /// stopped behind it, ssh exits 255 at once instead of dialing the old
    /// address (the caller's 255 rule then brings the member up again).
    pub fn ssh(&self) -> Command {
        let mut command = self.ssh_connect();
        command.args(NO_FALLBACK);
        command
    }

    /// `ssh` with this target's config that opens the control master itself
    /// when there is none. Only connecting (after a bring-up) uses it.
    pub fn ssh_connect(&self) -> Command {
        let mut command = Command::new("ssh");
        command.arg("-F").arg(&self.config);
        command
    }

    /// `ssh` with this target's config on a connection of its own, outside
    /// the control master, that fails when a forward it carries cannot be
    /// set up. A server runs on one: its forwards end with it, its end never
    /// touches the shared connection, and the connection closing (the
    /// client died) ends the remote session.
    pub fn ssh_dedicated(&self) -> Command {
        let mut command = self.ssh_connect();
        command.args(DEDICATED);
        command
    }

    /// Whether the control master answers. False when no config exists.
    pub fn master_alive(&self) -> bool {
        if !self.config.exists() {
            return false;
        }
        self.ssh_connect()
            .args(["-O", "check", &self.alias])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Ask the control master to exit, if there is one.
    pub fn close_master(&self) {
        if !self.config.exists() {
            return;
        }
        let _ = self
            .ssh_connect()
            .args(["-O", "exit", &self.alias])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Run a short remote command, opening the master if needed. Returns the
    /// exit status and stderr.
    pub fn run_quiet(&self, remote: &str) -> Result<(i32, String), AppError> {
        let output = self
            .ssh_connect()
            .args(["-T", &self.alias, remote])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| AppError::io("cannot run ssh", error))?;
        Ok((
            output.status.code().unwrap_or(255),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }

    /// Delete the config and the pinned key.
    pub fn remove_files(&self) {
        let _ = fs::remove_file(&self.config);
        let _ = fs::remove_file(&self.known_hosts);
    }
}

/// Options for commands that must use the existing control master. A mux
/// client whose master is missing or fails connects by itself; the
/// `ProxyCommand` makes that fallback fail at once (exit 255) instead of
/// dialing an address the sandbox may no longer have.
pub const NO_FALLBACK: [&str; 4] = ["-o", "ControlMaster=no", "-o", "ProxyCommand=false"];

/// Options for a connection of its own (see [`SshTarget::ssh_dedicated`]).
pub const DEDICATED: [&str; 6] = [
    "-o",
    "ControlMaster=no",
    "-o",
    "ControlPath=none",
    "-o",
    "ExitOnForwardFailure=yes",
];

/// The OpenSSH config for one sandbox. Keepalives every 5 s, 3 missed:
/// a master whose sandbox stopped exits within about 15 to 20 s instead of
/// a minute, so it stops looking alive.
pub fn render_config(
    target: &SshTarget,
    access: &SshAccess,
    identity: &Path,
    control_dir: &Path,
) -> String {
    let lines = [
        format!(
            "# Written by boatparty for sandbox {}. Do not edit.",
            target.alias
        ),
        format!("Host {}", target.alias),
        format!("  HostName {}", access.host),
        format!("  Port {}", access.port),
        format!("  User {}", access.user),
        format!("  IdentityFile \"{}\"", identity.display()),
        "  IdentitiesOnly yes".to_string(),
        "  IdentityAgent none".to_string(),
        format!("  HostKeyAlias {}", target.alias),
        format!("  UserKnownHostsFile \"{}\"", target.known_hosts.display()),
        "  GlobalKnownHostsFile /dev/null".to_string(),
        "  StrictHostKeyChecking yes".to_string(),
        "  UpdateHostKeys no".to_string(),
        "  BatchMode yes".to_string(),
        "  ControlMaster auto".to_string(),
        format!("  ControlPath {}/%C", control_dir.display()),
        "  ControlPersist 60m".to_string(),
        "  ServerAliveInterval 5".to_string(),
        "  ServerAliveCountMax 3".to_string(),
        "  ConnectTimeout 15".to_string(),
        "  Compression no".to_string(),
        "  LogLevel ERROR".to_string(),
        String::new(),
    ];
    lines.join("\n")
}

/// The `known_hosts` line pinning the sandbox's host key under its alias.
pub fn render_known_hosts(target: &SshTarget, access: &SshAccess) -> String {
    format!("{} {}\n", target.alias, access.host_key.trim())
}

/// Write the pinned host key and the config for a sandbox, replacing both.
pub fn write_target(paths: &Paths, target: &SshTarget, access: &SshAccess) -> Result<(), AppError> {
    private_dir(&paths.ssh_state_dir())?;
    let control_dir = paths.control_dir();
    private_dir(&control_dir)?;
    write_atomic(&target.known_hosts, &render_known_hosts(target, access))?;
    write_atomic(
        &target.config,
        &render_config(target, access, &paths.ssh_key_file(), &control_dir),
    )
}

/// The tool's public key, generating the pair with `ssh-keygen` on first use.
pub fn ensure_key(paths: &Paths) -> Result<String, AppError> {
    let private = paths.ssh_key_file();
    let public = public_key_path(&private);
    if !(private.exists() && public.exists()) {
        let dir = private
            .parent()
            .expect("the key file always has a parent directory");
        private_dir(dir)?;
        // Serialize generation between processes with a lock of its own.
        let lock_path = dir.join("keygen.lock");
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| AppError::io(format!("cannot open {}", lock_path.display()), error))?;
        lock.lock()
            .map_err(|error| AppError::io(format!("cannot lock {}", lock_path.display()), error))?;
        if !(private.exists() && public.exists()) {
            let _ = fs::remove_file(&private);
            let _ = fs::remove_file(&public);
            let output = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "boatparty", "-f"])
                .arg(&private)
                .stdin(Stdio::null())
                .output()
                .map_err(|error| {
                    AppError::io("cannot run ssh-keygen", error)
                        .with_hint("install OpenSSH and run `boatparty doctor`")
                })?;
            if !output.status.success() {
                return Err(AppError::new(
                    "ssh",
                    format!(
                        "ssh-keygen failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ));
            }
        }
    }
    let key = fs::read_to_string(&public)
        .map_err(|error| AppError::io(format!("cannot read {}", public.display()), error))?;
    let key = key.trim().to_string();
    if key.is_empty() || key.contains('\n') {
        return Err(AppError::new(
            "ssh",
            format!("{} is not a public key", public.display()),
        ));
    }
    Ok(key)
}

fn public_key_path(private: &Path) -> PathBuf {
    let mut name = private.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

fn private_dir(dir: &Path) -> Result<(), AppError> {
    if !dir.is_dir() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|error| AppError::io(format!("cannot create {}", dir.display()), error))?;
    }
    // The control directory lives in a shared /tmp: refuse one that is not ours.
    let meta = fs::symlink_metadata(dir)
        .map_err(|error| AppError::io(format!("cannot inspect {}", dir.display()), error))?;
    use std::os::unix::fs::MetadataExt;
    if !meta.is_dir() || meta.uid() != crate::system::uid() {
        return Err(AppError::config(format!(
            "{} is not a directory owned by this user",
            dir.display()
        )));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| AppError::io(format!("cannot secure {}", dir.display()), error))?;
    }
    Ok(())
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), AppError> {
    let dir = path.parent().expect("a file always has a parent directory");
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    result.map_err(|error| {
        let _ = fs::remove_file(&temp);
        AppError::io(format!("cannot write {}", path.display()), error)
    })
}

/// Quote one word for a POSIX shell.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget {
            alias: "bx_23456789".into(),
            config: "/state/ssh/bx_23456789.conf".into(),
            known_hosts: "/state/ssh/bx_23456789.known_hosts".into(),
        }
    }

    fn access() -> SshAccess {
        SshAccess {
            host: "203.0.113.10".into(),
            port: 22001,
            user: "user".into(),
            host_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample".into(),
        }
    }

    #[test]
    fn config_pins_the_host_key_and_multiplexes() {
        let text = render_config(
            &target(),
            &access(),
            Path::new("/cfg/ssh/id_ed25519"),
            Path::new("/tmp/boatparty-501"),
        );
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for expected in [
            "Host bx_23456789",
            "HostName 203.0.113.10",
            "Port 22001",
            "User user",
            "IdentityFile \"/cfg/ssh/id_ed25519\"",
            "IdentitiesOnly yes",
            "HostKeyAlias bx_23456789",
            "UserKnownHostsFile \"/state/ssh/bx_23456789.known_hosts\"",
            "GlobalKnownHostsFile /dev/null",
            "StrictHostKeyChecking yes",
            "UpdateHostKeys no",
            "BatchMode yes",
            "ControlMaster auto",
            "ControlPath /tmp/boatparty-501/%C",
            "ControlPersist 60m",
            "ServerAliveInterval 5",
            "ServerAliveCountMax 3",
        ] {
            assert!(lines.contains(&expected), "missing {expected:?} in\n{text}");
        }
        assert!(!text.contains("StrictHostKeyChecking no"));
        assert!(!text.contains("accept-new"));
    }

    #[test]
    fn control_sockets_live_under_tmp_per_user() {
        let home = std::ffi::OsString::from("/home/u/bp");
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        let expected = format!("/tmp/boatparty-{}", crate::system::uid());
        assert_eq!(paths.control_dir(), PathBuf::from(&expected));
        // The longest socket path (dir + "/" + 40 hex digits of %C) fits macOS's 104 bytes.
        assert!(expected.len() + 1 + 40 < 104);
        let target = SshTarget::for_sandbox(&paths, "bx_23456789");
        assert_eq!(
            target.config,
            PathBuf::from("/home/u/bp/state/ssh/bx_23456789.conf")
        );
        assert_eq!(
            target.known_hosts,
            PathBuf::from("/home/u/bp/state/ssh/bx_23456789.known_hosts")
        );
    }

    #[test]
    fn commands_use_the_master_without_falling_back_and_only_connect_opens_one() {
        let target = target();
        let args: Vec<String> = target
            .ssh()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-F",
                "/state/ssh/bx_23456789.conf",
                "-o",
                "ControlMaster=no",
                "-o",
                "ProxyCommand=false"
            ]
        );
        let connect: Vec<String> = target
            .ssh_connect()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(connect, ["-F", "/state/ssh/bx_23456789.conf"]);
        let dedicated: Vec<String> = target
            .ssh_dedicated()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            dedicated,
            [
                "-F",
                "/state/ssh/bx_23456789.conf",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "ExitOnForwardFailure=yes"
            ]
        );
    }

    #[test]
    fn known_hosts_line_is_the_alias_and_key() {
        assert_eq!(
            render_known_hosts(&target(), &access()),
            "bx_23456789 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample\n"
        );
    }

    #[test]
    fn shell_quote_survives_single_quotes_and_spaces() {
        assert_eq!(shell_quote("uname"), "uname");
        assert_eq!(shell_quote("-a"), "-a");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }
}
