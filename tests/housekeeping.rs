//! `boatparty skill` and the leftovers `boatparty doctor` reports.

#![cfg(unix)] // These fixtures execute the Linux runner or POSIX tool shims locally.

mod common;

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use common::{TempDir, boatparty};
use serde_json::Value;

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

#[test]
fn skill_writes_one_file_with_this_version_and_installs_nothing() {
    let root = TempDir::new("skill");
    let home = root.path().join("home");
    // The user's home: where agents keep their skills. Nothing may land there.
    let user_home = root.path().join("user");
    fs::create_dir_all(user_home.join(".claude/skills")).unwrap();
    let output = boatparty(&home)
        .env("HOME", &user_home)
        .current_dir(root.path())
        .arg("skill")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = home.join("state/skill/boatparty/SKILL.md");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n", path.display())
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only if the user asks"), "{stderr}");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nname: boatparty\n"));
    assert!(text.contains(&format!("boatparty {}", env!("CARGO_PKG_VERSION"))));
    assert_eq!(
        files(&home),
        vec![path.clone()],
        "skill wrote more than the skill"
    );
    assert!(files(&user_home).is_empty(), "skill installed something");

    // --print: the same text on stdout, and no file.
    fs::remove_file(&path).unwrap();
    let printed = boatparty(&home)
        .env("HOME", &user_home)
        .args(["skill", "--print"])
        .output()
        .unwrap();
    assert!(printed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&printed.stdout).trim_end(),
        text.trim_end()
    );
    assert!(!path.exists());

    let json = boatparty(&home).args(["--json", "skill"]).output().unwrap();
    let document: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(document["path"], path.display().to_string());
}

/// A short directory for sockets (a socket path is limited to 104 bytes).
struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> Self {
        let path = PathBuf::from(format!("/tmp/bp-ctl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn doctor(home: &Path, sockets: &Path) -> Value {
    let output = boatparty(home)
        .env("BOATPARTY_CONTROL_DIR", sockets)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

fn check<'a>(document: &'a Value, name: &str) -> &'a Value {
    document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check: {document}"))
}

#[test]
fn doctor_reports_stale_ssh_configs_and_control_sockets() {
    let root = TempDir::new("doctor-stale");
    let home = root.path().join("home");
    let sockets = SocketDir::new();

    // Nothing there yet: both checks pass.
    let clean = doctor(&home, &sockets.0);
    assert_eq!(check(&clean, "ssh_configs")["status"], "ok");
    assert_eq!(check(&clean, "control_sockets")["status"], "ok");

    // A config and host key for a sandbox no record names.
    let ssh = home.join("state/ssh");
    fs::create_dir_all(&ssh).unwrap();
    fs::write(ssh.join("bx_gone1234.conf"), "Host bx_gone1234\n").unwrap();
    fs::write(ssh.join("bx_gone1234.known_hosts"), "bx_gone1234 key\n").unwrap();
    // A socket a dead master left, and one a live master answers on.
    let stale = sockets.0.join("deadbeef");
    drop(UnixListener::bind(&stale).unwrap());
    let live_path = sockets.0.join("cafef00d");
    let live = UnixListener::bind(&live_path).unwrap();

    let document = doctor(&home, &sockets.0);
    let configs = check(&document, "ssh_configs");
    assert_eq!(configs["status"], "warn", "{configs}");
    let detail = configs["detail"].as_str().unwrap();
    assert!(detail.contains("bx_gone1234.conf") && detail.contains("bx_gone1234.known_hosts"));
    let fix = configs["fix"].as_str().unwrap();
    assert!(
        fix.starts_with("rm -f ") && fix.contains("bx_gone1234.conf"),
        "{fix}"
    );

    let sockets_check = check(&document, "control_sockets");
    assert_eq!(sockets_check["status"], "warn", "{sockets_check}");
    let detail = sockets_check["detail"].as_str().unwrap();
    assert!(
        detail.contains("deadbeef") && !detail.contains("cafef00d"),
        "{detail}"
    );
    assert!(sockets_check["fix"].as_str().unwrap().contains("deadbeef"));
    // Warnings are not failures: the document fails only for the missing key.
    assert_eq!(check(&document, "credentials")["status"], "fail");
    drop(live);
}
