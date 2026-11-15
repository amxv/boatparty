//! `init`, `launch`, `board`, `status`, `dock` and `rm` against a fake boat
//! API and fake `ssh`/`ssh-keygen` executables.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{FakeBoat, Reply, Request, TempDir, boatparty, store_key};
use serde_json::{Value, json};

const ID: &str = "bx_23456789";

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is one JSON document")
}

/// A Git repository with `boatparty init` already run, plus a home and tools.
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
        setup.tools();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn repo(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path().join("app")).unwrap()
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn ssh_log(&self) -> PathBuf {
        self.root.path().join("ssh.log")
    }

    /// Fake `ssh` (logs its arguments; `-O check` answers $FAKE_SSH_CHECK,
    /// other calls exit $FAKE_SSH_EXIT), fake `ssh-keygen`, and the real `git`.
    fn tools(&self) {
        let bin = self.bin();
        std::fs::create_dir_all(&bin).unwrap();
        let ssh = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
             for a in \"$@\"; do if [ \"$a\" = check ]; then exit \"${{FAKE_SSH_CHECK:-1}}\"; fi; done\n\
             for a in \"$@\"; do last=\"$a\"; done\n\
             case \"$last\" in *'&&'*) printf 'remote: %s\\n' \"$last\";; esac\n\
             exit \"${{FAKE_SSH_EXIT:-0}}\"\n",
            self.ssh_log().display()
        );
        let keygen = "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -f ]; then f=\"$2\"; fi; shift; done\n\
                      printf 'private\\n' > \"$f\"\nprintf 'ssh-ed25519 AAAAfakekey boatparty\\n' > \"$f.pub\"\n";
        for (name, body) in [("ssh", ssh.as_str()), ("ssh-keygen", keygen)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let git = which("git");
        std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    }

    fn cmd(&self, boat: &FakeBoat) -> assert_cmd::Command {
        let mut command = boatparty(&self.home());
        command
            .current_dir(self.repo())
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("PATH", self.bin())
            .env_remove("FAKE_SSH_CHECK")
            .env_remove("FAKE_SSH_EXIT");
        command
    }

    fn init(&self) {
        boatparty(&self.home())
            .current_dir(self.repo())
            .env("PATH", self.bin())
            .args(["init"])
            .assert()
            .success();
        // A lease shorter than the idle length, so the idle deadline (300 s)
        // is told apart from a lease deadline (240 s).
        let account = self.home().join("config/account.toml");
        let text = std::fs::read_to_string(&account).unwrap();
        std::fs::write(
            &account,
            text.replace("lease_minutes = 5", "lease_minutes = 4"),
        )
        .unwrap();
    }

    /// Wait (up to 10 s) for an idle deadline (`ttlSeconds` 300) among the
    /// requests from index `from`: the detached idle helper's.
    fn wait_idle(boat: &FakeBoat, from: usize) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if boat.requests()[from.min(boat.requests().len())..]
                .iter()
                .any(|request| request.method == "PATCH" && request.body.contains("300"))
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    fn state(&self) -> Value {
        let text = std::fs::read_to_string(self.home().join("state/state.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn which(tool: &str) -> PathBuf {
    let output = std::process::Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn sandbox(state: &str, hydrated: Option<bool>) -> Value {
    let mut sandbox = json!({
        "id": ID, "name": "bp-app-1", "state": state,
        "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
        "type": "default", "archiveAfter": "2026-11-15T12:15:00Z",
        "desktopAvailable": false, "snapshotAvailable": false
    });
    if let Some(hydrated) = hydrated {
        sandbox["hydrated"] = json!(hydrated);
    }
    sandbox
}

fn info(state: &str, hydrated: Option<bool>) -> Reply {
    Reply::json(
        200,
        &json!({"ok": true, "type": "sandbox.info", "sandbox": sandbox(state, hydrated)})
            .to_string(),
    )
}

fn created() -> Reply {
    Reply::json(
        202,
        &json!({"ok": true, "type": "sandbox.created", "status": "provisioning",
                "ttlSeconds": 900, "sandbox": sandbox("provisioning", None)})
        .to_string(),
    )
}

fn ssh_key(host_key: &str) -> Reply {
    Reply::json(
        200,
        &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                "machineIp": "198.51.100.7", "sshUser": "user",
                "sshEndpoint": "203.0.113.10:22001", "hostKey": host_key})
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

fn read_pending(home: &Path) -> Value {
    let text = std::fs::read_to_string(home.join("state/state.json")).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A fake for a member that is created, then running and hydrated.
fn running_boat(home: PathBuf, seen_at_create: Arc<Mutex<Vec<Value>>>) -> FakeBoat {
    FakeBoat::start(move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/sandboxes") => {
                // What was on disk at the moment the create arrived.
                seen_at_create.lock().unwrap().push(read_pending(&home));
                created()
            }
            ("GET", path) if path.starts_with("/sandboxes?") => Reply::json(
                200,
                &json!({"ok": true, "type": "sandbox.list",
                        "sandboxes": [sandbox("idle", Some(true))],
                        "pageInfo": {"nextCursor": null, "hasMore": false, "limit": 200}})
                .to_string(),
            ),
            ("GET", path) if path == format!("/sandboxes/{ID}") => info("idle", Some(true)),
            ("POST", path) if path == format!("/sandboxes/{ID}/sshkey") => {
                ssh_key("ssh-ed25519 AAAAhostkeyone")
            }
            ("PATCH", _) => info("idle", Some(true)),
            _ => Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#),
        }
    })
}

#[test]
fn init_writes_the_project_file_and_registers_it() {
    let setup = Setup::new("init");
    let output = boatparty(&setup.home())
        .current_dir(setup.repo())
        .env("PATH", setup.bin())
        .args(["init", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["project"], "app");
    assert_eq!(document["file_created"], true);
    assert_eq!(document["registered"], true);
    let file = std::fs::read_to_string(setup.repo().join("boatparty.toml")).unwrap();
    assert!(file.contains("version = 1") && file.contains("name = \"app\""));
    let account = std::fs::read_to_string(setup.home().join("config/account.toml")).unwrap();
    assert!(account.ends_with("[projects.app]\n"), "{account}");

    // Again: nothing changes.
    let output = boatparty(&setup.home())
        .current_dir(setup.repo())
        .env("PATH", setup.bin())
        .args(["init", "--json"])
        .output()
        .unwrap();
    let document = parse_json(&output.stdout);
    assert_eq!(document["file_created"], false);
    assert_eq!(document["registered"], false);
    // A different name for an initialized project is refused.
    boatparty(&setup.home())
        .current_dir(setup.repo())
        .env("PATH", setup.bin())
        .args(["init", "--name", "other"])
        .assert()
        .code(125);
}

#[test]
fn init_rejects_bad_names_and_non_git_directories() {
    let setup = Setup::new("init-bad");
    boatparty(&setup.home())
        .current_dir(setup.repo())
        .env("PATH", setup.bin())
        .args(["init", "--name", "Bad_Name"])
        .assert()
        .code(125);
    assert!(!setup.repo().join("boatparty.toml").exists());
    let plain = setup.root.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let output = boatparty(&setup.home())
        .current_dir(&plain)
        .env("PATH", setup.bin())
        .env("GIT_CEILING_DIRECTORIES", setup.root.path())
        .args(["init", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "workspace");
}

#[test]
fn launch_records_the_pending_member_before_creating_it() {
    let setup = Setup::new("launch");
    setup.init();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let boat = running_boat(setup.home(), Arc::clone(&seen));
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["member"]["name"], "bp-app-1");
    assert_eq!(document["member"]["id"], ID);
    assert_eq!(document["member"]["created"], true);

    let requests = boat.requests();
    let creates: Vec<&Request> = requests
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/sandboxes")
        .collect();
    assert_eq!(creates.len(), 1);
    let body: Value = serde_json::from_str(&creates[0].body).unwrap();
    assert_eq!(body["noEnv"], true);
    assert_eq!(body["name"], "bp-app-1");
    assert_eq!(body["type"], "default");
    assert_eq!(body["labels"]["boatparty.project"], "app");
    assert_eq!(body["labels"]["boatparty.role"], "pool");
    // Created with the lease length, then left at the idle length by the
    // detached helper, since launch holds no lease.
    assert_eq!(body["ttlSeconds"], 240);
    assert!(Setup::wait_idle(&boat, 0), "launch set no idle deadline");
    assert!(body.get("env").is_none() && body.get("environment").is_none());
    let key = header(creates[0], "idempotency-key").expect("Idempotency-Key sent");

    // The pending record, with that key and no id, was on disk before the call.
    let before = seen.lock().unwrap()[0].clone();
    let pending = &before["projects"]["app"]["members"][0];
    assert_eq!(pending["create_key"], key);
    assert_eq!(pending["id"], Value::Null);
    assert_eq!(pending["name"], "bp-app-1");
    assert_eq!(pending["affinity"], setup.repo().display().to_string());

    // Afterwards the record carries the id and the same key.
    let state = setup.state();
    let member = &state["projects"]["app"]["members"][0];
    assert_eq!(member["id"], ID);
    assert_eq!(member["create_key"], key);
    assert_eq!(member["bringing_up"], Value::Null);

    // The tool's key was authorized and the host key pinned.
    let sshkey = requests
        .iter()
        .find(|request| request.path.ends_with("/sshkey"))
        .unwrap();
    assert!(sshkey.body.contains("ssh-ed25519 AAAAfakekey"));
    let ssh_dir = setup.home().join("state/ssh");
    let known = std::fs::read_to_string(ssh_dir.join(format!("{ID}.known_hosts"))).unwrap();
    assert_eq!(known, format!("{ID} ssh-ed25519 AAAAhostkeyone\n"));
    let config = std::fs::read_to_string(ssh_dir.join(format!("{ID}.conf"))).unwrap();
    assert!(config.contains("HostName 203.0.113.10") && config.contains("Port 22001"));
    assert!(config.contains("StrictHostKeyChecking yes"));
    let log = std::fs::read_to_string(setup.ssh_log()).unwrap();
    assert!(log.contains("mkdir -p /home/user/.boatparty"), "{log}");
}

#[test]
fn a_lost_create_answer_is_retried_with_the_same_key() {
    let setup = Setup::new("launch-retry");
    setup.init();
    let attempts = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&attempts);
    let boat =
        FakeBoat::start(
            move |request| match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/sandboxes") => {
                    let mut n = counter.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        Reply::json(502, r#"{"ok":false,"code":"http_502","status":502}"#)
                    } else {
                        created()
                    }
                }
                ("GET", path) if path.starts_with(&format!("/sandboxes/{ID}")) => {
                    info("ready", Some(true))
                }
                ("POST", path) if path.ends_with("/sshkey") => ssh_key("ssh-ed25519 AAAAk"),
                _ => Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#),
            },
        );
    setup.cmd(&boat).arg("launch").assert().success();
    let requests = boat.requests();
    let keys: Vec<&str> = requests
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/sandboxes")
        .map(|request| header(request, "idempotency-key").unwrap())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    let state = setup.state();
    assert_eq!(
        state["projects"]["app"]["members"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_refused_create_drops_the_pending_record() {
    let setup = Setup::new("launch-refused");
    setup.init();
    // A refusal that a retry cannot fix (rate limits are retried, see
    // tests/starts.rs).
    let refused = common::fixture("errors/402-billing-required.json");
    let boat = FakeBoat::start(move |request| match request.method.as_str() {
        "POST" => Reply::json(402, &refused),
        _ => Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#),
    });
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "billing");
    let state = setup.state();
    assert_eq!(state["projects"]["app"]["members"], json!([]));
    // A refused start leaves the ledger unchanged.
    assert_eq!(state["starts"], json!([]));
}

#[test]
fn an_unknown_provider_state_is_an_error_and_is_never_resumed() {
    let setup = Setup::new("launch-unknown");
    setup.init();
    let boat =
        FakeBoat::start(
            move |request| match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/sandboxes") => created(),
                ("GET", _) => info("hibernating", None),
                _ => Reply::json(500, r#"{"ok":false,"code":"http_500","status":500}"#),
            },
        );
    let output = setup
        .cmd(&boat)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        parse_json(&output.stdout)["error"]["code"],
        "invalid_response"
    );
    assert!(
        !boat
            .requests()
            .iter()
            .any(|request| request.path.ends_with("/resume"))
    );
}

#[test]
fn board_on_a_warm_member_makes_no_synchronous_provider_call_and_returns_the_exit_code() {
    let setup = Setup::new("board");
    setup.init();
    let boat = running_boat(setup.home(), Arc::new(Mutex::new(Vec::new())));
    setup.cmd(&boat).arg("launch").assert().success();
    // launch leaves the idle deadline to its helper too; let it finish.
    assert!(Setup::wait_idle(&boat, 0), "launch set no idle deadline");
    let before = boat.requests().len();
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("FAKE_SSH_EXIT", "7")
        .args(["board", "--", "sh", "-c", "echo it's"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(r"cd /home/user/app && sh -c 'echo it'\''s'"),
        "{stdout}"
    );
    // Only the background heartbeat and the detached idle helper talk to the
    // provider, and only to read the member and move its auto-stop.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && !boat.requests()[before..]
            .iter()
            .any(|request| request.method == "PATCH" && request.body.contains("300"))
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let later = boat.requests()[before..].to_vec();
    assert!(
        later
            .iter()
            .any(|request| request.method == "PATCH" && request.body.contains("300")),
        "the idle deadline was never set: {later:?}"
    );
    for request in &later {
        assert_eq!(request.path, format!("/sandboxes/{ID}"), "{request:?}");
        assert!(
            matches!(request.method.as_str(), "GET" | "PATCH"),
            "{request:?}"
        );
    }
    let state = setup.state();
    assert_eq!(state["projects"]["app"]["members"][0]["leases"], json!([]));
}

#[test]
fn status_json_describes_each_member() {
    let setup = Setup::new("status");
    setup.init();
    let boat = running_boat(setup.home(), Arc::new(Mutex::new(Vec::new())));
    setup.cmd(&boat).arg("launch").assert().success();
    let output = setup
        .cmd(&boat)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    let member = &document["members"][0];
    assert_eq!(member["id"], ID);
    assert_eq!(member["state"], "running");
    assert_eq!(member["hydration"], "complete");
    assert_eq!(member["affinity"], setup.repo().display().to_string());
    assert_eq!(member["this_workspace"], true);
    assert_eq!(member["auto_stop_at"], "2026-11-15T12:15:00Z");
    assert_eq!(member["ssh"]["host_alias"], ID);
    let config = member["ssh"]["config"].as_str().unwrap();
    assert!(config.ends_with(&format!("state/ssh/{ID}.conf")));
    assert_eq!(member["ssh"]["configured"], true);
    assert_eq!(document["untracked"], json!([]));
}

#[test]
fn dock_waits_for_hydration_before_stopping() {
    let setup = Setup::new("dock");
    setup.init();
    let polls = Arc::new(Mutex::new(0));
    let stopped = Arc::new(Mutex::new(false));
    let (poll_count, stop_flag) = (Arc::clone(&polls), Arc::clone(&stopped));
    let boat = FakeBoat::start(move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/sandboxes") => created(),
            ("POST", path) if path.ends_with("/sshkey") => ssh_key("ssh-ed25519 AAAAk"),
            ("POST", path) if path.ends_with("/stop") => {
                *stop_flag.lock().unwrap() = true;
                Reply::json(
                    202,
                    r#"{"ok":true,"type":"sandbox.stopping","id":"bx_23456789","status":"archiving"}"#,
                )
            }
            ("GET", _) => {
                if *stop_flag.lock().unwrap() {
                    return info("archived", Some(true));
                }
                let mut n = poll_count.lock().unwrap();
                *n += 1;
                // Running at once; files unknown, then arriving, then complete.
                match *n {
                    1..=2 => info("ready", None),
                    3..=5 => info("ready", Some(false)),
                    _ => info("ready", Some(true)),
                }
            }
            _ => info("ready", Some(true)),
        }
    });
    setup.cmd(&boat).arg("launch").assert().success();
    let output = setup
        .cmd(&boat)
        .args(["dock", "--wait", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["stop_requested"], true);
    assert_eq!(document["member"]["state"], "stopped");
    // The stop came only after the provider reported the files complete.
    let requests = boat.requests();
    let stop_at = requests
        .iter()
        .position(|request| request.path.ends_with("/stop"))
        .unwrap();
    let gets_before_stop = requests[..stop_at]
        .iter()
        .filter(|request| request.method == "GET")
        .count();
    assert!(
        gets_before_stop >= 6,
        "stopped after {gets_before_stop} polls"
    );
}

#[test]
fn dock_and_rm_refuse_a_member_with_a_live_lease() {
    let setup = Setup::new("leased");
    setup.init();
    let boat = running_boat(setup.home(), Arc::new(Mutex::new(Vec::new())));
    setup.cmd(&boat).arg("launch").assert().success();
    let mut state = setup.state();
    state["projects"]["app"]["members"][0]["leases"] = json!([
        {"pid": std::process::id(), "kind": "run", "label": "cargo test", "started_at": 0}
    ]);
    std::fs::write(
        setup.home().join("state/state.json"),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    let before = boat.requests().len();
    for command in ["dock", "rm"] {
        let output = setup.cmd(&boat).args([command, "--json"]).output().unwrap();
        assert_eq!(output.status.code(), Some(125));
        assert_eq!(parse_json(&output.stdout)["error"]["code"], "member_in_use");
    }
    let calls: Vec<String> = boat.requests()[before..]
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("/stop") && !call.starts_with("DELETE")),
        "{calls:?}"
    );
}

#[test]
fn rm_waits_for_the_deletion_and_drops_the_record() {
    let setup = Setup::new("rm");
    setup.init();
    let polls = Arc::new(Mutex::new(0));
    let poll_count = Arc::clone(&polls);
    let operation = "bdop_8c86906803da420b85becdfc03862f30";
    let boat = FakeBoat::start(move |request| {
        let op = |status: &str| {
            json!({"ok": true, "type": "deletion.operation", "operation": {
                "id": operation, "kind": "sandbox", "targetId": ID, "reason": "explicit",
                "status": status, "attemptCount": 1, "requestedAt": "2026-11-15T00:00:00Z",
                "completedAt": null}})
            .to_string()
        };
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/sandboxes") => created(),
            ("POST", path) if path.ends_with("/sshkey") => ssh_key("ssh-ed25519 AAAAk"),
            ("DELETE", path) if path == format!("/sandboxes/{ID}") => {
                Reply::json(202, &op("processing"))
            }
            ("GET", path) if path.starts_with("/deletion-operations/") => {
                let mut n = poll_count.lock().unwrap();
                *n += 1;
                Reply::json(200, &op(if *n < 3 { "blocked" } else { "completed" }))
            }
            _ => info("ready", Some(true)),
        }
    });
    setup.cmd(&boat).arg("launch").assert().success();
    let output = setup.cmd(&boat).args(["rm", "--json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["deleted"], true);
    assert_eq!(document["deletion"]["status"], "completed");
    let requests = boat.requests();
    let delete = requests
        .iter()
        .find(|request| request.method == "DELETE")
        .unwrap();
    assert_eq!(header(delete, "x-ascii-confirm-delete"), Some(ID));
    assert_eq!(*polls.lock().unwrap(), 3);
    assert_eq!(setup.state()["projects"]["app"]["members"], json!([]));
    assert!(!setup.home().join(format!("state/ssh/{ID}.conf")).exists());
}

#[test]
fn launch_resumes_a_stopped_member_and_pins_the_new_host_key() {
    let setup = Setup::new("resume");
    setup.init();
    let resumed = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&resumed);
    let boat = FakeBoat::start(move |request| {
        let is_resumed = *flag.lock().unwrap();
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/sandboxes") => created(),
            ("POST", path) if path.ends_with("/resume") => {
                *flag.lock().unwrap() = true;
                Reply::json(
                    202,
                    &json!({"ok": true, "type": "sandbox.resuming", "id": ID,
                            "status": "resuming", "sandbox": sandbox("provisioned", Some(false))})
                    .to_string(),
                )
            }
            ("POST", path) if path.ends_with("/sshkey") => ssh_key(if is_resumed {
                "ssh-ed25519 AAAAhostkeytwo"
            } else {
                "ssh-ed25519 AAAAhostkeyone"
            }),
            ("GET", _) if is_resumed => info("ready", Some(false)),
            ("GET", _) => info("archived", Some(true)),
            _ => info("ready", Some(true)),
        }
    });
    // A record for a stopped member, as an earlier launch and dock leave it.
    let state = json!({
        "version": 1,
        "projects": {"app": {"members": [{
            "id": ID, "create_key": "k-1", "name": "bp-app-1", "role": "pool",
            "affinity": setup.repo().display().to_string(), "leases": [],
            "bringing_up": null, "sync": {"mode": "head", "known": [], "refs_digest": null},
            "created_at": 0, "last_used_at": 0, "last_run": null
        }], "base": null}},
        "starts": [], "limits_cache": null
    });
    std::fs::create_dir_all(setup.home().join("state/ssh")).unwrap();
    std::fs::write(setup.home().join("state/state.json"), state.to_string()).unwrap();
    std::fs::write(
        setup.home().join(format!("state/ssh/{ID}.known_hosts")),
        format!("{ID} ssh-ed25519 AAAAhostkeyone\n"),
    )
    .unwrap();
    std::fs::write(
        setup.home().join(format!("state/ssh/{ID}.conf")),
        format!("Host {ID}\n  HostName 198.51.100.1\n"),
    )
    .unwrap();

    let output = setup
        .cmd(&boat)
        // Even a master that looks alive is replaced after a start.
        .env("FAKE_SSH_CHECK", "0")
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["member"]["resumed"], true);
    assert_eq!(document["member"]["reclaimed"], false);
    assert_eq!(document["member"]["reconnected"], true);
    let resume = boat
        .requests()
        .into_iter()
        .find(|request| request.path.ends_with("/resume"))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&resume.body).unwrap()["ttlSeconds"],
        240
    );
    let known =
        std::fs::read_to_string(setup.home().join(format!("state/ssh/{ID}.known_hosts"))).unwrap();
    assert_eq!(known, format!("{ID} ssh-ed25519 AAAAhostkeytwo\n"));
    let log = std::fs::read_to_string(setup.ssh_log()).unwrap();
    assert!(log.contains(&format!("-O exit {ID}")), "{log}");
    assert!(
        !boat
            .requests()
            .iter()
            .any(|request| request.method == "POST" && request.path == "/sandboxes")
    );
}
