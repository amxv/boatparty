//! The shared pool: waiting at the cap, release and assign, reassignment,
//! reconciliation by label and prune, against a fake boat API that keeps
//! several sandboxes, with fake `ssh` and `ssh-keygen`.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{FakeBoat, Reply, Request, TempDir, boatparty, store_key};
use serde_json::{Value, json};

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is one JSON document")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// One sandbox the fake knows.
#[derive(Clone)]
struct Fake {
    name: String,
    state: &'static str,
    expired: bool,
    deleted: bool,
}

type Sandboxes = Arc<Mutex<BTreeMap<String, Fake>>>;

fn sandbox_json(id: &str, fake: &Fake) -> Value {
    json!({
        "id": id, "name": fake.name, "state": fake.state, "hydrated": true,
        "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
        "type": "default", "archiveAfter": "2026-11-15T12:15:00Z",
        "dataExpiredAt": if fake.expired { json!("2026-11-01T00:00:00Z") } else { Value::Null },
    })
}

fn not_found() -> Reply {
    Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#)
}

/// A fake boat API holding `sandboxes`: list by label, get, create, resume,
/// sshkey, TTL changes and asynchronous deletion.
fn pool_boat(sandboxes: Sandboxes) -> FakeBoat {
    FakeBoat::start(move |request: &Request| {
        let mut all = sandboxes.lock().unwrap();
        let path = request
            .path
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let operation = |id: &str, status: &str| {
            json!({"ok": true, "type": "deletion.operation", "operation": {
                "id": format!("bdop_{id}"), "kind": "sandbox", "targetId": id,
                "reason": "explicit", "status": status, "attemptCount": 1,
                "requestedAt": "2026-11-15T00:00:00Z", "completedAt": null}})
            .to_string()
        };
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["sandboxes"]) => {
                let listed: Vec<Value> = all
                    .iter()
                    .filter(|(_, fake)| !fake.deleted)
                    .map(|(id, fake)| sandbox_json(id, fake))
                    .collect();
                Reply::json(
                    200,
                    &json!({"ok": true, "type": "sandbox.list", "sandboxes": listed,
                            "pageInfo": {"nextCursor": null, "hasMore": false, "limit": 200}})
                    .to_string(),
                )
            }
            ("POST", ["sandboxes"]) => {
                let body: Value = serde_json::from_str(&request.body).unwrap();
                let id = format!("bx_new{}", all.len() + 1);
                let fake = Fake {
                    name: body["name"].as_str().unwrap().to_string(),
                    state: "idle",
                    expired: false,
                    deleted: false,
                };
                let sandbox = sandbox_json(&id, &fake);
                all.insert(id, fake);
                Reply::json(
                    202,
                    &json!({"ok": true, "type": "sandbox.created", "status": "provisioning",
                            "ttlSeconds": 900, "sandbox": sandbox})
                    .to_string(),
                )
            }
            ("GET", ["deletion-operations", op]) => {
                let id = op.trim_start_matches("bdop_");
                Reply::json(200, &operation(id, "completed"))
            }
            (method, ["sandboxes", id, rest @ ..]) => {
                let Some(fake) = all.get_mut(*id).filter(|fake| !fake.deleted) else {
                    return not_found();
                };
                match (method, rest) {
                    ("GET" | "PATCH", []) => Reply::json(
                        200,
                        &json!({"ok": true, "type": "sandbox.info",
                                "sandbox": sandbox_json(id, fake)})
                        .to_string(),
                    ),
                    ("DELETE", []) => {
                        fake.deleted = true;
                        Reply::json(202, &operation(id, "processing"))
                    }
                    ("POST", ["resume"]) => {
                        fake.state = "idle";
                        Reply::json(
                            200,
                            &json!({"ok": true, "type": "sandbox.resumed", "id": id,
                                    "status": "running", "sandbox": sandbox_json(id, fake)})
                            .to_string(),
                        )
                    }
                    ("POST", ["sshkey"]) => Reply::json(
                        200,
                        &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                                "machineIp": "198.51.100.7", "sshUser": "user",
                                "sshEndpoint": "203.0.113.10:22001",
                                "hostKey": "ssh-ed25519 AAAAhostkey"})
                        .to_string(),
                    ),
                    _ => not_found(),
                }
            }
            _ => not_found(),
        }
    })
}

fn fakes(entries: &[(&str, &str, &'static str, bool)]) -> Sandboxes {
    Arc::new(Mutex::new(
        entries
            .iter()
            .map(|&(id, name, state, expired)| {
                (
                    id.to_string(),
                    Fake {
                        name: name.to_string(),
                        state,
                        expired,
                        deleted: false,
                    },
                )
            })
            .collect(),
    ))
}

/// One home shared by several workspaces of project `app`.
struct Setup {
    root: TempDir,
}

impl Setup {
    fn new(label: &str) -> Self {
        let setup = Self {
            root: TempDir::new(label),
        };
        store_key(&setup.home());
        let bin = setup.bin();
        std::fs::create_dir_all(&bin).unwrap();
        let ssh = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
             for a in \"$@\"; do if [ \"$a\" = check ]; then exit \"${{FAKE_SSH_CHECK:-1}}\"; fi; done\n\
             exit 0\n",
            setup.root.path().join("ssh.log").display()
        );
        let keygen = "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -f ]; then f=\"$2\"; fi; shift; done\n\
                      printf 'private\\n' > \"$f\"\nprintf 'ssh-ed25519 AAAAfakekey boatparty\\n' > \"$f.pub\"\n";
        for (name, body) in [("ssh", ssh.as_str()), ("ssh-keygen", keygen)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let git = std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        std::os::unix::fs::symlink(text(&git.stdout).trim(), bin.join("git")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    /// A Git repository `name` with `boatparty init --name app` run.
    fn workspace(&self, name: &str) -> PathBuf {
        let repo = self.root.path().join(name);
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        boatparty(&self.home())
            .current_dir(&repo)
            .env("PATH", self.bin())
            .args(["init", "--name", "app"])
            .assert()
            .success();
        std::fs::canonicalize(repo).unwrap()
    }

    fn cmd(&self, boat: &FakeBoat, dir: &Path) -> assert_cmd::Command {
        let mut command = boatparty(&self.home());
        command
            .current_dir(dir)
            .env("BOATPARTY_BOAT_API_URL", boat.url())
            .env("BOATPARTY_POLL_MS", "10")
            .env("PATH", self.bin())
            .env_remove("FAKE_SSH_CHECK");
        command
    }

    fn account_append(&self, text: &str) {
        let path = self.home().join("config/account.toml");
        let mut account = std::fs::read_to_string(&path).unwrap();
        account.push_str(text);
        std::fs::write(path, account).unwrap();
    }

    fn state(&self) -> Value {
        let text = std::fs::read_to_string(self.home().join("state/state.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn write_members(&self, members: Value) {
        let state = json!({
            "version": 1,
            "projects": {"app": {"members": members, "base": null}},
            "starts": [], "limits_cache": null
        });
        std::fs::create_dir_all(self.home().join("state")).unwrap();
        std::fs::write(self.home().join("state/state.json"), state.to_string()).unwrap();
    }
}

fn record(id: &str, name: &str, affinity: Option<&Path>) -> Value {
    json!({
        "id": id, "create_key": format!("k-{id}"), "name": name, "role": "pool",
        "affinity": affinity.map(|path| path.display().to_string()),
        "leases": [], "bringing_up": null,
        "sync": {"mode": "head", "known": [], "refs_digest": null},
        "created_at": 0, "last_used_at": 1, "last_run": null
    })
}

fn deletes(boat: &FakeBoat) -> Vec<String> {
    boat.requests()
        .iter()
        .filter(|request| request.method == "DELETE")
        .map(|request| request.path.clone())
        .collect()
}

#[test]
fn a_second_workspace_waits_at_the_cap_and_is_served_after_release() {
    let setup = Setup::new("pool-cap");
    let first = setup.workspace("first");
    let second = setup.workspace("second");
    // A per-project cap of one, and a short wait.
    setup.account_append("max_sandboxes = 1\n");
    let path = setup.home().join("config/account.toml");
    let account = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        account.replace("acquire_wait_seconds = 900", "acquire_wait_seconds = 1"),
    )
    .unwrap();
    let boat = pool_boat(fakes(&[]));
    setup.cmd(&boat, &first).arg("launch").assert().success();

    // The first workspace holds a lease, as a running `cheers` would.
    let mut holder = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut state = setup.state();
    state["projects"]["app"]["members"][0]["leases"] = json!([{
        "pid": holder.id(), "kind": "run", "label": "sleep 60", "started_at": 0,
        "workspace": first.display().to_string()
    }]);
    std::fs::write(
        setup.home().join("state/state.json"),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let output = setup
        .cmd(&boat, &second)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125), "{output:?}");
    let error = &parse_json(&output.stdout)["error"];
    assert_eq!(error["code"], "pool_exhausted");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("max_sandboxes is 1"), "{message}");
    assert!(message.contains("[projects.app]"), "{message}");
    assert!(
        text(&output.stderr).contains("waiting up to 1s"),
        "{}",
        text(&output.stderr)
    );
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));

    // Release is refused under the live lease.
    let output = setup
        .cmd(&boat, &first)
        .args(["release", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "member_in_use");

    // The lease ends; release, then the second workspace is served.
    let _ = holder.kill();
    let _ = holder.wait();
    let output = setup
        .cmd(&boat, &first)
        .args(["release", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["released"], true);
    assert_eq!(document["member"]["name"], "bp-app-1");

    let output = setup
        .cmd(&boat, &second)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(parse_json(&output.stdout)["member"]["name"], "bp-app-1");
    assert!(text(&output.stderr).contains("using the free sandbox bp-app-1"));

    let output = setup
        .cmd(&boat, &first)
        .args(["pool", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let pool = &parse_json(&output.stdout)["projects"][0];
    assert_eq!(pool["project"], "app");
    assert_eq!(pool["max_sandboxes"], 1);
    assert_eq!(pool["members"].as_array().unwrap().len(), 1);
    assert_eq!(pool["members"][0]["affinity"], second.display().to_string());
    assert_eq!(pool["members"][0]["state"], "running");

    // The second workspace's directory disappears: the first takes the
    // member over, and nothing is deleted.
    std::fs::remove_dir_all(&second).unwrap();
    let output = setup
        .cmd(&boat, &first)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        text(&output.stderr).contains("taking over bp-app-1"),
        "{}",
        text(&output.stderr)
    );
    let creates = boat
        .requests()
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/sandboxes")
        .count();
    assert_eq!(creates, 1);
    assert!(deletes(&boat).is_empty(), "{:?}", deletes(&boat));
}

#[test]
fn an_idle_member_of_another_workspace_is_taken_after_steal_after() {
    let setup = Setup::new("pool-steal");
    let first = setup.workspace("first");
    let second = setup.workspace("second");
    setup.account_append("max_sandboxes = 1\n");
    let path = setup.home().join("config/account.toml");
    let account = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        account
            .replace("acquire_wait_seconds = 900", "acquire_wait_seconds = 0")
            .replace("steal_after_minutes = 30", "steal_after_minutes = 1"),
    )
    .unwrap();
    let boat = pool_boat(fakes(&[("bx_1", "bp-app-1", "archived", false)]));
    // Used two minutes ago by the first workspace.
    let mut member = record("bx_1", "bp-app-1", Some(&first));
    member["last_used_at"] = json!(now_ms() - 120_000);
    setup.write_members(json!([member]));
    let output = setup
        .cmd(&boat, &second)
        .args(["launch", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("unused for more than 1 minutes"),
        "{stderr}"
    );
    assert_eq!(
        setup.state()["projects"]["app"]["members"][0]["affinity"],
        second.display().to_string()
    );
    // It was stopped, so it was resumed rather than replaced.
    assert!(
        boat.requests()
            .iter()
            .any(|request| request.path.ends_with("/resume"))
    );
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn assign_and_release_only_change_records() {
    let setup = Setup::new("pool-assign");
    let first = setup.workspace("first");
    let second = setup.workspace("second");
    setup.write_members(json!([
        record("bx_1", "bp-app-1", Some(&first)),
        record("bx_2", "bp-app-2", None),
    ]));
    let boat = pool_boat(fakes(&[]));
    let output = setup
        .cmd(&boat, &first)
        .args(["assign", "bp-app-2", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    assert_eq!(document["member"]["name"], "bp-app-2");
    assert_eq!(document["previous"]["name"], "bp-app-1");

    // bp-app-2 belongs to an existing workspace now.
    let output = setup
        .cmd(&boat, &second)
        .args(["assign", "bx_2", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        parse_json(&output.stdout)["error"]["code"],
        "member_assigned"
    );
    // bp-app-1 is free.
    setup
        .cmd(&boat, &second)
        .args(["assign", "bp-app-1"])
        .assert()
        .success();
    let state = setup.state();
    let members = state["projects"]["app"]["members"].as_array().unwrap();
    assert_eq!(members[0]["affinity"], second.display().to_string());
    assert_eq!(members[1]["affinity"], first.display().to_string());

    let output = setup
        .cmd(&boat, &second)
        .args(["release", "--json"])
        .output()
        .unwrap();
    assert_eq!(parse_json(&output.stdout)["released"], true);
    let output = setup
        .cmd(&boat, &second)
        .args(["release", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(parse_json(&output.stdout)["released"], false);
    assert!(
        boat.requests().is_empty(),
        "release and assign called the provider"
    );
}

#[test]
fn pool_refresh_adopts_drops_deletes_expired_and_never_deletes_for_a_missing_path() {
    let setup = Setup::new("pool-refresh");
    let here = setup.workspace("here");
    let missing = setup.root.path().join("moved-away");
    setup.write_members(json!([
        record("bx_gone", "bp-app-1", Some(&here)),
        record("bx_old", "bp-app-3", None),
        record("bx_orphan", "bp-app-4", Some(&missing)),
    ]));
    let boat = pool_boat(fakes(&[
        ("bx_untracked", "bp-app-2", "idle", false),
        ("bx_old", "bp-app-3", "archived", true),
        ("bx_orphan", "bp-app-4", "archived", false),
    ]));
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "--refresh", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let pool = &parse_json(&output.stdout)["projects"][0];
    let changes = &pool["changes"];
    assert_eq!(changes["adopted"][0]["id"], "bx_untracked");
    assert_eq!(changes["adopted"][0]["name"], "bp-app-2");
    assert_eq!(changes["dropped"][0]["id"], "bx_gone");
    assert_eq!(changes["expired"][0]["id"], "bx_old");
    assert_eq!(changes["freed"][0][0]["id"], "bx_orphan");
    assert_eq!(changes["freed"][0][1], missing.display().to_string());
    // Only the data-expired member was deleted, and its deletion was awaited.
    assert_eq!(deletes(&boat), ["/sandboxes/bx_old"]);
    assert!(
        boat.requests()
            .iter()
            .any(|request| request.path == "/deletion-operations/bdop_bx_old")
    );
    let state = setup.state();
    let members: Vec<(&str, &Value)> = state["projects"]["app"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| (member["id"].as_str().unwrap(), &member["affinity"]))
        .collect();
    assert_eq!(
        members,
        [("bx_orphan", &Value::Null), ("bx_untracked", &Value::Null)]
    );

    // Running it again changes nothing.
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "--refresh", "--json"])
        .output()
        .unwrap();
    let changes = &parse_json(&output.stdout)["projects"][0]["changes"];
    for list in ["adopted", "dropped", "expired", "freed"] {
        assert_eq!(changes[list], json!([]), "{list}");
    }
    assert_eq!(deletes(&boat).len(), 1);
}

#[test]
fn prune_asks_first_and_spares_members_of_existing_workspaces() {
    let setup = Setup::new("pool-prune");
    let here = setup.workspace("here");
    let missing = setup.root.path().join("deleted-worktree");
    setup.write_members(json!([
        record("bx_free", "bp-app-1", None),
        record("bx_mine", "bp-app-2", Some(&here)),
        record("bx_orphan", "bp-app-3", Some(&missing)),
        record("bx_up", "bp-app-4", None),
    ]));
    let boat = pool_boat(fakes(&[
        ("bx_free", "bp-app-1", "archived", false),
        ("bx_mine", "bp-app-2", "archived", false),
        ("bx_orphan", "bp-app-3", "archived", false),
        ("bx_up", "bp-app-4", "idle", false),
    ]));

    // Declined: nothing is deleted.
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "prune"])
        .write_stdin("n\n")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("delete them? [y/N]"), "{stderr}");
    assert!(
        stderr.contains("bp-app-1") && stderr.contains("bp-app-3"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("bp-app-2") && !stderr.contains("bp-app-4"),
        "{stderr}"
    );
    assert!(text(&output.stdout).contains("nothing was deleted"));
    assert!(deletes(&boat).is_empty());

    // No answer at all declines too.
    setup
        .cmd(&boat, &here)
        .args(["pool", "prune"])
        .write_stdin("")
        .assert()
        .success();
    assert!(deletes(&boat).is_empty());

    // Agreed: the free and the orphaned stopped members go, with their
    // deletions awaited.
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "prune", "--json"])
        .write_stdin("y\n")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document = parse_json(&output.stdout);
    let deleted: Vec<&str> = document["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["id"].as_str().unwrap())
        .collect();
    assert_eq!(deleted, ["bx_free", "bx_orphan"]);
    assert_eq!(
        deletes(&boat),
        ["/sandboxes/bx_free", "/sandboxes/bx_orphan"]
    );
    let polls = boat
        .requests()
        .iter()
        .filter(|request| request.path.starts_with("/deletion-operations/"))
        .count();
    assert_eq!(polls, 2);
    let ids: Vec<String> = setup.state()["projects"]["app"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["bx_mine", "bx_up"]);

    // --yes needs no answer; nothing is left to prune.
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "prune", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(parse_json(&output.stdout)["candidates"], json!([]));
    assert_eq!(deletes(&boat).len(), 2);
}

#[test]
fn prune_yes_deletes_without_asking() {
    let setup = Setup::new("pool-prune-yes");
    let here = setup.workspace("here");
    setup.write_members(json!([record("bx_free", "bp-app-1", None)]));
    let boat = pool_boat(fakes(&[("bx_free", "bp-app-1", "archived", false)]));
    let output = setup
        .cmd(&boat, &here)
        .args(["pool", "prune", "--yes"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!text(&output.stderr).contains("[y/N]"));
    assert!(text(&output.stdout).contains("deleted app bp-app-1 (bx_free)"));
    assert_eq!(deletes(&boat), ["/sandboxes/bx_free"]);
}

// ----- the warm path with other members in the pool ----------------------

use common::e2e::{E2e, ID};

#[test]
fn a_warm_cheers_with_other_members_in_the_pool_makes_no_provider_call() {
    let setup = E2e::new("pool-warm");
    let boat = setup.boat();
    setup.launch(&boat);
    // Another workspace's member and a free one share the pool.
    let file = setup.home().join("state/state.json");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let members = state["projects"]["app"]["members"].as_array_mut().unwrap();
    members.push(record(
        "bx_other",
        "bp-app-2",
        Some(Path::new("/elsewhere")),
    ));
    members.push(record("bx_free", "bp-app-3", None));
    std::fs::write(&file, serde_json::to_string(&state).unwrap()).unwrap();
    let _ = std::fs::remove_file(setup.events());

    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let lines = setup.event_lines();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("ssh-run"),
        "{lines:#?}"
    );
    for line in lines.iter().filter(|line| line.starts_with("provider")) {
        assert!(
            line.starts_with(&format!("provider PATCH /sandboxes/{ID}"))
                || line.starts_with(&format!("provider GET /sandboxes/{ID}")),
            "unexpected provider call {line}"
        );
    }
    // The lease named this workspace while it was held, and is gone now.
    let state: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(state["projects"]["app"]["members"][0]["leases"], json!([]));
    assert_eq!(
        state["projects"]["app"]["members"][1]["affinity"],
        "/elsewhere"
    );
}

#[test]
fn launch_and_load_leave_the_member_at_the_idle_deadline() {
    let setup = E2e::new("pool-idle");
    let boat = setup.boat();
    // Created with the lease length (4 minutes in this home) ...
    setup.launch(&boat);
    let lines = setup.event_lines();
    let create = lines
        .iter()
        .find(|line| line.starts_with("provider POST /sandboxes "))
        .unwrap();
    assert!(create.contains("\"ttlSeconds\":240"), "{create}");
    // ... then the idle length, set by the helper launch leaves behind.
    let last_patch = |setup: &E2e| {
        setup
            .event_lines()
            .into_iter()
            .rfind(|line| line.starts_with("provider PATCH"))
            .unwrap()
    };
    assert!(last_patch(&setup).ends_with("{\"ttlSeconds\":300}"));

    // A warm load holds a lease while it syncs, and the last leaver's
    // helper sets the idle deadline again.
    let before = setup.patches();
    setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .arg("load")
        .assert()
        .success();
    assert!(setup.wait_patches(before), "load set no idle deadline");
    assert!(last_patch(&setup).ends_with("{\"ttlSeconds\":300}"));
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(setup.home().join("state/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["projects"]["app"]["members"][0]["leases"], json!([]));
}
