//! Native Windows CLI, Windows OpenSSH and Git for Windows against a real
//! Linux sshd and runner. CI provisions the disposable Linux test server.
#![cfg(windows)]

mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use common::runner::git;
use common::{FakeBoat, Reply, TEST_KEY, TempDir, boatparty, fixture};
use serde_json::{Value, json};

const ID: &str = "bx_23456789";

fn provider(host_key: String) -> FakeBoat {
    let state = Mutex::new(None);
    FakeBoat::start(move |request| {
        let mut state = state.lock().unwrap();
        let sandbox = |state: &str| {
            json!({
                "id": ID, "name": "bp-app-1", "type": "default", "state": state, "hydrated": true,
                "labels": {"boatparty.project": "app", "boatparty.role": "pool"}
            })
        };
        let reply = match (
            request.method.as_str(),
            request.path.split('?').next().unwrap(),
        ) {
            ("GET", "/limits") => return Reply::json(200, &fixture("limits.json")),
            ("GET", "/sandboxes") => {
                json!({"sandboxes": state.iter().copied().map(sandbox).collect::<Vec<_>>()})
            }
            ("POST", "/sandboxes") if request.body.contains("scope-probe") => {
                return Reply::json(400, &fixture("errors/400-invalid-type.json"));
            }
            ("POST", "/sandboxes") => {
                *state = Some("idle");
                json!({"sandbox": sandbox("idle")})
            }
            ("GET" | "PATCH", "/sandboxes/bx_23456789") => {
                json!({"sandbox": sandbox(state.unwrap())})
            }
            ("POST", "/sandboxes/bx_23456789/stop") => {
                *state = Some("archived");
                json!({"ok": true})
            }
            ("POST", "/sandboxes/bx_23456789/sshkey") => {
                let request: Value = serde_json::from_str(&request.body).unwrap();
                let mut writer = Command::new("wsl.exe")
                    .args([
                        "-u",
                        "root",
                        "--",
                        "tee",
                        "/run/boatparty-test/authorized_keys",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                writeln!(
                    writer.stdin.take().unwrap(),
                    "{}",
                    request["key"].as_str().unwrap()
                )
                .unwrap();
                assert!(writer.wait().unwrap().success());
                json!({"sshUser": "user", "sshEndpoint": "127.0.0.1:22222", "hostKey": host_key})
            }
            _ => return Reply::json(404, &fixture("errors/404-not-found.json")),
        };
        Reply::json(200, &reply.to_string())
    })
}

#[test]
#[ignore = "requires the disposable WSL sshd fixture from the Windows CI job"]
fn native_client_runs_the_linux_checkout() {
    let host_key = std::env::var("BOATPARTY_TEST_HOST_KEY").unwrap();
    let mut sshd = Command::new("wsl.exe")
        .args([
            "-u",
            "root",
            "--",
            "/usr/sbin/sshd",
            "-D",
            "-e",
            "-p",
            "22222",
            "-h",
            "/etc/ssh/ssh_host_ed25519_key",
            "-o",
            "ListenAddress=127.0.0.1",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "UsePAM=no",
            "-o",
            "AuthorizedKeysFile=/run/boatparty-test/authorized_keys",
        ])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let boat = provider(host_key);
    let root = TempDir::new("Windows client");
    let home = root.path().join("boatparty home");
    let repo = root.path().join("checkout with spaces");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("tracked"), "original\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    let cli = || {
        let mut cmd = boatparty(&home);
        cmd.current_dir(&repo)
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "100");
        cmd
    };
    cli()
        .args(["login", "--key-stdin"])
        .write_stdin(format!("{TEST_KEY}\r\n"))
        .assert()
        .success();
    cli().arg("doctor").assert().success();
    cli().args(["init", "--name", "app"]).assert().success();
    cli().arg("start").assert().success();
    // Include binary data and exercise warming an already configured target.
    let binary: Vec<u8> = (0..=255).cycle().take(64 * 1024).collect();
    fs::write(repo.join("input.bin"), &binary).unwrap();
    cli().arg("sync").assert().success();
    cli()
        .args(["run", "--sh", "cp input.bin result.bin; echo done; exit 7"])
        .assert()
        .code(7)
        .stdout("done\n");
    assert_eq!(fs::read(repo.join("result.bin")).unwrap(), binary);
    cli()
        .args(["shell", "--", "cat", "tracked"])
        .assert()
        .success()
        .stdout("original\n");
    cli().arg("status").assert().success();
    cli().args(["stop", "--wait"]).assert().success();
    assert!(
        boat.requests()
            .iter()
            .any(|req| req.path.ends_with("/stop"))
    );
    let _ = sshd.kill();
    let _ = sshd.wait();
}
