//! The start ledger and the limits in account.toml, against a fake boat API
//! and fake `ssh`/`ssh-keygen`: rate-limited starts are retried, the day
//! window fails, refused and reclaimed starts give their reservation back,
//! project and account limits hold a start back with one log line, and
//! `limits` / `limits set` read and edit the limits.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{FakeBoat, Reply, Request, TempDir, boatparty, store_key};
use serde_json::{Value, json};

const ID: &str = "bx_23456789";

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is one JSON document")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

struct Setup {
    root: TempDir,
}

impl Setup {
    fn new(label: &str) -> Self {
        let root = TempDir::new(label);
        let repo = root.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let status = common::isolated("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let setup = Self { root };
        store_key(&setup.home());
        let bin = setup.root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ssh =
            "#!/bin/sh\nfor a in \"$@\"; do if [ \"$a\" = check ]; then exit 1; fi; done\nexit 0\n";
        let keygen = "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -f ]; then f=\"$2\"; fi; shift; done\n\
                      printf 'private\\n' > \"$f\"\nprintf 'ssh-ed25519 AAAAfakekey boatparty\\n' > \"$f.pub\"\n";
        for (name, body) in [("ssh", ssh), ("ssh-keygen", keygen)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        std::os::unix::fs::symlink(text(&git.stdout).trim(), bin.join("git")).unwrap();
        boatparty(&setup.home())
            .current_dir(setup.repo())
            .env("PATH", &bin)
            .args(["init", "--name", "app"])
            .assert()
            .success();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn repo(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path().join("app")).unwrap()
    }

    fn account_file(&self) -> PathBuf {
        self.home().join("config/account.toml")
    }

    /// Append lines to the end of account.toml (inside `[projects.app]`).
    fn project_limits(&self, lines: &str) {
        let mut account = std::fs::read_to_string(self.account_file()).unwrap();
        account.push_str(lines);
        std::fs::write(self.account_file(), account).unwrap();
    }

    fn cmd(&self, boat: &FakeBoat) -> assert_cmd::Command {
        let mut command = boatparty(&self.home());
        command
            .current_dir(self.repo())
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("PATH", self.root.path().join("bin"));
        command
    }

    fn state(&self) -> Value {
        let text = std::fs::read_to_string(self.home().join("state/state.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// A stopped member of this workspace, plus `starts` in the ledger.
    fn stopped_member(&self, starts: Value) {
        let state = json!({
            "version": 1,
            "projects": {"app": {"members": [{
                "id": ID, "create_key": "k-1", "name": "bp-app-1", "role": "pool",
                "affinity": self.repo().display().to_string(),
                "leases": [], "bringing_up": null,
                "sync": {"mode": "head", "known": [], "refs_digest": null},
                "created_at": 0, "last_used_at": 1, "last_run": null
            }], "base": null}},
            "starts": starts, "limits_cache": null
        });
        std::fs::create_dir_all(self.home().join("state")).unwrap();
        std::fs::write(self.home().join("state/state.json"), state.to_string()).unwrap();
    }

    /// Start `launch`, wait for a stderr line containing `needle`, then
    /// cancel it with SIGINT. Returns everything it printed on stderr.
    fn launch_until(&self, boat: &FakeBoat, needle: &str) -> String {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_boatparty"))
            .arg("launch")
            .current_dir(self.repo())
            .env("BOATPARTY_HOME", self.home())
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("PATH", self.root.path().join("bin"))
            .env_remove("BOATPARTY_BOAT_API_KEY")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !seen.contains(needle) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match received.recv_timeout(left) {
                Ok(line) => {
                    seen.push_str(&line);
                    seen.push('\n');
                }
                Err(_) => break,
            }
        }
        // SIGINT ends a waiting start. A test runner can start this test
        // with SIGINT ignored (inherited), so it falls back to SIGKILL.
        let _ = std::process::Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status();
        let stop_by = std::time::Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() && std::time::Instant::now() < stop_by {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let status = child.wait().unwrap();
        assert!(seen.contains(needle), "never printed {needle:?}:\n{seen}");
        assert!(!status.success(), "launch finished instead of waiting");
        seen
    }
}

fn sandbox(state: &str) -> Value {
    json!({
        "id": ID, "name": "bp-app-1", "state": state, "hydrated": true,
        "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
        "type": "default", "archiveAfter": "2026-11-15T12:15:00Z"
    })
}

fn info(state: &str) -> Reply {
    Reply::json(
        200,
        &json!({"ok": true, "type": "sandbox.info", "sandbox": sandbox(state)}).to_string(),
    )
}

fn not_found() -> Reply {
    Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#)
}

fn limits_reply(hour_used: u64, day_used: u64) -> Reply {
    Reply::json(
        200,
        &json!({"canStart": true, "activeSandboxes": 1, "maxActiveSandboxes": 100,
                "billingStatus": "active",
                "starts": {"unlimited": false,
                           "minute": {"limit": 12, "used": 0, "remaining": 12},
                           "hour": {"limit": 60, "used": hour_used, "remaining": 60 - hour_used},
                           "day": {"limit": 200, "used": day_used, "remaining": 200 - day_used}},
                "planIncluded": null, "machineTypes": []})
        .to_string(),
    )
}

fn rate_limited(window: &str, retry_after: u64) -> Reply {
    Reply::json(
        429,
        &json!({"ok": false, "type": "sandbox.error", "status": 429, "code": "rate_limited",
                "message": format!("You used all your starts this {window}."),
                "window": window, "limit": 12, "cause": "plan",
                "nextStartAt": "2026-11-15T00:31:41.000Z", "retryAfterSeconds": retry_after,
                "error": {"code": "rate_limited", "message": "full", "status": 429}})
        .to_string(),
    )
    .header("Retry-After", &retry_after.to_string())
}

/// Answers every call of an ordinary bring-up; `create` answers creates.
fn boat(create: impl Fn(usize) -> Reply + Send + Sync + 'static) -> FakeBoat {
    let creates = AtomicUsize::new(0);
    FakeBoat::start(move |request: &Request| {
        let path = request.path.split('?').next().unwrap_or_default();
        match (request.method.as_str(), path) {
            ("POST", "/sandboxes") => create(creates.fetch_add(1, Ordering::SeqCst)),
            ("GET", "/limits") => limits_reply(8, 50),
            ("GET", "/sandboxes") => Reply::json(
                200,
                &json!({"ok": true, "type": "sandbox.list", "sandboxes": [],
                        "pageInfo": {"nextCursor": null, "hasMore": false, "limit": 200}})
                .to_string(),
            ),
            ("POST", path) if path.ends_with("/sshkey") => Reply::json(
                200,
                &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                        "machineIp": "198.51.100.7", "sshUser": "user",
                        "sshEndpoint": "203.0.113.10:22001", "hostKey": "ssh-ed25519 AAAAk"})
                .to_string(),
            ),
            ("GET" | "PATCH", path) if path == format!("/sandboxes/{ID}") => info("idle"),
            _ => not_found(),
        }
    })
}

fn created() -> Reply {
    Reply::json(
        202,
        &json!({"ok": true, "type": "sandbox.created", "status": "provisioning",
                "ttlSeconds": 300, "sandbox": sandbox("provisioning")})
        .to_string(),
    )
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn a_rate_limited_create_is_retried_after_the_providers_delay_and_never_surfaces() {
    let setup = Setup::new("starts-retry");
    let boat = boat(|n| {
        if n == 0 {
            rate_limited("minute", 1)
        } else {
            created()
        }
    });
    let started = std::time::Instant::now();
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(started.elapsed() >= Duration::from_secs(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("refused starting bp-app-1") && stderr.contains("retrying in 2s"),
        "{stderr}"
    );
    assert_eq!(parse_json(&output.stdout)["member"]["id"], ID);

    // The retry used a new idempotency key, and the record carries it.
    let requests = boat.requests();
    let keys: Vec<&str> = requests
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/sandboxes")
        .map(|request| header(request, "idempotency-key").unwrap())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    let state = setup.state();
    let member = &state["projects"]["app"]["members"][0];
    assert_eq!(member["id"], ID);
    assert_eq!(member["create_key"], keys[1]);
    // One start in the ledger: the refused one gave its reservation back.
    let starts = state["starts"].as_array().unwrap();
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts[0]["project"], "app");
    assert!(starts[0]["done_at"].as_u64().is_some());
    assert!(starts[0].get("pid").is_none());
    // The provider's figures were cached before the start.
    assert_eq!(state["limits_cache"]["hour"]["used"], 8);
}

#[test]
fn the_day_window_fails_with_a_message_naming_it() {
    let setup = Setup::new("starts-day");
    let boat = boat(|_| rate_limited("day", 3600));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125), "{output:?}");
    let error = &parse_json(&output.stdout)["error"];
    assert_eq!(error["code"], "daily_start_limit");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("daily start limit"), "{message}");
    let creates = boat
        .requests()
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/sandboxes")
        .count();
    assert_eq!(creates, 1, "the day window is not retried");
    let state = setup.state();
    assert_eq!(state["projects"]["app"]["members"], json!([]));
    assert_eq!(state["starts"], json!([]));
}

/// A boat whose one sandbox is stopped until resumed; `resume` answers.
fn stopped_boat(resume: Reply) -> (FakeBoat, Arc<AtomicBool>) {
    let resumed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&resumed);
    let boat = FakeBoat::start(move |request: &Request| {
        let path = request.path.split('?').next().unwrap_or_default();
        match (request.method.as_str(), path) {
            ("GET", "/limits") => limits_reply(8, 50),
            ("POST", path) if path.ends_with("/resume") => {
                if resume.status < 300 {
                    flag.store(true, Ordering::SeqCst);
                }
                resume.clone()
            }
            ("POST", path) if path.ends_with("/sshkey") => Reply::json(
                200,
                &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                        "machineIp": "198.51.100.7", "sshUser": "user",
                        "sshEndpoint": "203.0.113.10:22001", "hostKey": "ssh-ed25519 AAAAk"})
                .to_string(),
            ),
            ("GET" | "PATCH", path) if path == format!("/sandboxes/{ID}") => {
                info(if flag.load(Ordering::SeqCst) {
                    "idle"
                } else {
                    "archived"
                })
            }
            _ => not_found(),
        }
    });
    (boat, resumed)
}

fn resumed_reply(status: u16, outcome: &str, state: &str) -> Reply {
    Reply::json(
        status,
        &json!({"ok": true, "type": "sandbox.resuming", "id": ID, "status": outcome,
                "sandbox": sandbox(state)})
        .to_string(),
    )
}

#[test]
fn a_reclaim_resume_releases_its_reservation_and_a_new_machine_keeps_it() {
    let setup = Setup::new("starts-reclaim");
    setup.stopped_member(json!([]));
    let (boat, _) = stopped_boat(resumed_reply(200, "running", "ready"));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(parse_json(&output.stdout)["member"]["reclaimed"], true);
    assert_eq!(setup.state()["starts"], json!([]));

    let setup = Setup::new("starts-new-machine");
    setup.stopped_member(json!([]));
    let (boat, _) = stopped_boat(resumed_reply(202, "resuming", "provisioned"));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(parse_json(&output.stdout)["member"]["reclaimed"], false);
    let state = setup.state();
    assert_eq!(state["starts"].as_array().unwrap().len(), 1);
    assert_eq!(state["starts"][0]["project"], "app");
}

#[test]
fn a_refused_resume_leaves_the_ledger_unchanged() {
    let setup = Setup::new("starts-refused");
    let earlier =
        json!([{"at": now_ms() - 60_000, "project": "app", "done_at": now_ms() - 59_000}]);
    setup.stopped_member(earlier.clone());
    let refused = common::fixture("errors/402-billing-required.json");
    let (boat, _) = stopped_boat(Reply::json(402, &refused));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "billing");
    assert_eq!(setup.state()["starts"], earlier);
}

#[test]
fn a_project_hour_cap_waits_with_one_line_and_starts_nothing() {
    let setup = Setup::new("starts-project-hour");
    setup.project_limits("starts_per_hour = 1\n");
    let earlier =
        json!([{"at": now_ms() - 60_000, "project": "app", "done_at": now_ms() - 59_000}]);
    setup.stopped_member(earlier.clone());
    let (boat, _) = stopped_boat(resumed_reply(202, "resuming", "provisioned"));
    let stderr = setup.launch_until(&boat, "waits for project app's hourly start limit");
    assert!(stderr.contains("starts_per_hour = 1"), "{stderr}");
    assert!(stderr.contains("about 59m"), "{stderr}");
    assert_eq!(
        stderr.matches("waits for").count(),
        1,
        "one log line: {stderr}"
    );
    assert!(
        !boat
            .requests()
            .iter()
            .any(|request| request.path.ends_with("/resume")),
        "a start was sent"
    );
    assert_eq!(setup.state()["starts"], earlier);
}

#[test]
fn a_project_day_cap_refuses_at_once_naming_the_limit() {
    let setup = Setup::new("starts-project-day");
    setup.project_limits("starts_per_day = 1\n");
    let earlier = json!([{"at": now_ms() - 2 * 3_600_000, "project": "app", "done_at": now_ms()}]);
    setup.stopped_member(earlier.clone());
    let (boat, _) = stopped_boat(resumed_reply(202, "resuming", "provisioned"));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125), "{output:?}");
    let error = &parse_json(&output.stdout)["error"];
    assert_eq!(error["code"], "daily_start_limit");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("starts_per_day = 1") && message.contains("account.toml"),
        "{message}"
    );
    assert!(message.contains("about 22h"), "{message}");
    assert_eq!(setup.state()["starts"], earlier);
}

#[test]
fn the_account_reserve_holds_a_start_back() {
    let setup = Setup::new("starts-reserve");
    let account = std::fs::read_to_string(setup.account_file()).unwrap();
    std::fs::write(
        setup.account_file(),
        account.replace(
            "reserve_starts_per_hour = 0",
            "reserve_starts_per_hour = 52",
        ),
    )
    .unwrap();
    setup.stopped_member(json!([]));
    // The provider says 8 of 60 used this hour: 52 left, all in reserve.
    let (boat, _) = stopped_boat(resumed_reply(202, "resuming", "provisioned"));
    let stderr = setup.launch_until(&boat, "waits for the account's hourly starts");
    assert!(
        stderr.contains("8 of 60 used, 52 kept in reserve (reserve_starts_per_hour)"),
        "{stderr}"
    );
    assert_eq!(setup.state()["starts"], json!([]));
}

#[test]
fn a_machine_above_max_machine_is_refused_naming_both_files() {
    let setup = Setup::new("starts-machine");
    setup.project_limits("max_machine = \"default\"\n");
    let project = setup.repo().join("boatparty.toml");
    let mut text = std::fs::read_to_string(&project).unwrap();
    text.push_str("\n[sandbox]\nmachine = \"large\"\n");
    std::fs::write(&project, text).unwrap();
    let boat = boat(|_| created());
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let error = &parse_json(&output.stdout)["error"];
    assert_eq!(error["code"], "config");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("boatparty.toml") && message.contains("account.toml"),
        "{message}"
    );
    assert!(message.contains("max_machine"), "{message}");
    assert!(boat.requests().is_empty());

    // Asking for less than the maximum creates that size.
    let text = std::fs::read_to_string(&project)
        .unwrap()
        .replace("\"large\"", "\"small\"");
    std::fs::write(&project, text).unwrap();
    setup.cmd(&boat).arg("launch").assert().success();
    let create = boat
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == "/sandboxes")
        .unwrap();
    let body: Value = serde_json::from_str(&create.body).unwrap();
    assert_eq!(body["type"], "small");
}

#[test]
fn limits_set_edits_account_toml_in_place_and_limits_shows_usage() {
    let setup = Setup::new("limits-set");
    let before = std::fs::read_to_string(setup.account_file()).unwrap();
    let boat = boat(|_| created());
    let output = setup
        .cmd(&boat)
        .args([
            "limits",
            "set",
            "project",
            "app",
            "starts_per_hour=2",
            "max_running=1",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["changes"][0]["table"], "projects.app");
    assert_eq!(document["changes"][0]["new"], 2);
    let after = std::fs::read_to_string(setup.account_file()).unwrap();
    // Everything before the project's table is untouched, comments included.
    let head = before.trim_end_matches("[projects.app]\n");
    assert!(after.starts_with(head), "{after}");
    assert!(
        after.ends_with("[projects.app]\nstarts_per_hour = 2\nmax_running = 1\n"),
        "{after}"
    );

    // Above a cap: refused, file unchanged.
    let output = setup
        .cmd(&boat)
        .args([
            "limits",
            "set",
            "project",
            "app",
            "lease_minutes=30",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let message = parse_json(&output.stdout)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("lease_minutes"), "{message}");
    assert_eq!(
        std::fs::read_to_string(setup.account_file()).unwrap(),
        after
    );

    let output = setup
        .cmd(&boat)
        .args(["limits", "set", "account", "reserve_starts_per_day=5"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    // A start of this project in the ledger shows as used.
    let state = json!({"version": 1, "projects": {}, "limits_cache": null,
                       "starts": [{"at": now_ms() - 1_000, "project": "app", "done_at": now_ms()}]});
    std::fs::create_dir_all(setup.home().join("state")).unwrap();
    std::fs::write(setup.home().join("state/state.json"), state.to_string()).unwrap();
    let output = setup
        .cmd(&boat)
        .args(["limits", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["starts"]["hour"]["used"], 8);
    assert_eq!(document["account"]["reserve_starts_per_day"], 5);
    assert_eq!(document["account"]["boatparty_starts"]["hour"], 1);
    let project = &document["projects"][0];
    assert_eq!(project["project"], "app");
    assert_eq!(project["starts"]["hour"]["used"], 1);
    assert_eq!(project["starts"]["hour"]["cap"], 2);
    assert_eq!(project["max_running"], 1);
    assert_eq!(project["running"], 0);
    assert!(
        boat.requests()
            .iter()
            .any(|request| request.path.contains("include=cost")),
        "the cost was not asked for"
    );
}
