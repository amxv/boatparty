//! The base snapshot end to end, against a fake boat API that keeps
//! sandboxes and saved snapshots, with the fake `ssh` running the runner
//! locally (every sandbox shares the mapped /home/user/, as a member made
//! from a saved snapshot shares its builder's disk).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};

use common::contract::assert_shape;
use common::e2e::E2e;
use common::runner::git;
use common::{FakeBoat, Reply, Request};
use serde_json::{Value, json};

#[derive(Clone)]
struct Box_ {
    name: String,
    role: String,
    state: &'static str,
    deleted: bool,
}

#[derive(Default)]
struct World {
    sandboxes: BTreeMap<String, Box_>,
    /// name -> (status, polls left before the final status)
    snapshots: BTreeMap<String, (String, u32)>,
    /// The builder's files never become complete.
    hydrate_never: bool,
    /// What the next save ends as.
    save_ends: String,
    created: u32,
    /// name -> the artifact generation its `snapshotId` names.
    generations: BTreeMap<String, u32>,
    /// After a save under an existing ready name, how many polls still show
    /// the old artifact as ready (a stale answer).
    stale_polls: u32,
    /// name -> stale polls left.
    stale: BTreeMap<String, u32>,
}

type Shared = Arc<Mutex<World>>;

fn sandbox_json(id: &str, sandbox: &Box_, hydrated: bool) -> Value {
    json!({
        "id": id, "name": sandbox.name, "state": sandbox.state, "hydrated": hydrated,
        "labels": {"boatparty.project": "app", "boatparty.role": sandbox.role},
        "type": "default", "archiveAfter": "2026-11-15T12:15:00Z",
    })
}

fn snapshot_json(name: &str, status: &str, generation: u32) -> Value {
    json!({"name": name, "status": status, "sourceSandboxId": "bx_b", "sizeBytes": 4096,
           "createdAt": "2026-11-15T00:00:00Z", "type": "default",
           "snapshotId": format!("snap-{generation}")})
}

fn not_found() -> Reply {
    Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#)
}

/// The fake boat, logging every call to the e2e events file in order.
fn boat(setup: &E2e, world: Shared) -> FakeBoat {
    let events = setup.events();
    FakeBoat::start(move |request: &Request| {
        let path = request
            .path
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        {
            let mut log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&events)
                .unwrap();
            let body = request.body.split_whitespace().collect::<String>();
            let line = format!("provider {} {} {}\n", request.method, path, body);
            let _ = log.write_all(line.as_bytes());
        }
        let mut world = world.lock().unwrap();
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let operation = |id: &str| {
            json!({"ok": true, "type": "deletion.operation", "operation": {
                "id": format!("bdop_{id}"), "kind": "sandbox", "targetId": id,
                "reason": "explicit", "status": "completed", "attemptCount": 1,
                "requestedAt": "2026-11-15T00:00:00Z", "completedAt": null}})
            .to_string()
        };
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["sandboxes"]) => {
                let hydrate_never = world.hydrate_never;
                let listed: Vec<Value> = world
                    .sandboxes
                    .iter()
                    .filter(|(_, sandbox)| !sandbox.deleted)
                    .map(|(id, sandbox)| {
                        sandbox_json(id, sandbox, !(hydrate_never && sandbox.role == "builder"))
                    })
                    .collect();
                Reply::json(
                    200,
                    &json!({"ok": true, "sandboxes": listed,
                            "pageInfo": {"nextCursor": null, "hasMore": false}})
                    .to_string(),
                )
            }
            ("POST", ["sandboxes"]) => {
                let body: Value = serde_json::from_str(&request.body).unwrap();
                world.created += 1;
                let role = body["labels"]["boatparty.role"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let id = if role == "builder" {
                    "bx_b".to_string()
                } else {
                    format!("bx_p{}", world.created)
                };
                let sandbox = Box_ {
                    name: body["name"].as_str().unwrap().to_string(),
                    role,
                    state: "idle",
                    deleted: false,
                };
                let json = sandbox_json(&id, &sandbox, true);
                world.sandboxes.insert(id, sandbox);
                Reply::json(
                    202,
                    &json!({"ok": true, "status": "provisioning", "sandbox": json}).to_string(),
                )
            }
            ("GET", ["deletion-operations", op]) => {
                Reply::json(200, &operation(op.trim_start_matches("bdop_")))
            }
            ("POST", ["saved-snapshots"]) => {
                let body: Value = serde_json::from_str(&request.body).unwrap();
                let name = body["name"].as_str().unwrap().to_string();
                let ends = world.save_ends.clone();
                let replacing = world
                    .snapshots
                    .get(&name)
                    .is_some_and(|(status, _)| status == "ready");
                if replacing {
                    let stale = world.stale_polls;
                    world.stale.insert(name.clone(), stale);
                }
                let generation = world.generations.get(&name).copied().unwrap_or(0) + 1;
                world.generations.insert(name.clone(), generation);
                world.snapshots.insert(name.clone(), (ends, 1));
                Reply::json(
                    202,
                    &json!({"ok": true, "status": "saving",
                            "snapshot": snapshot_json(&name, "saving", generation - 1)})
                    .to_string(),
                )
            }
            ("GET", ["saved-snapshots"]) => {
                let listed: Vec<Value> = world
                    .snapshots
                    .iter()
                    .map(|(name, (status, _))| snapshot_json(name, status, world.generations[name]))
                    .collect();
                Reply::json(
                    200,
                    &json!({"ok": true, "snapshots": listed, "allowance": {
                        "included": 10, "used": listed.len(), "extra": 0,
                        "extraMonthlyDollars": 0.0, "pricePerExtraDailyDollars": 0.056}})
                    .to_string(),
                )
            }
            ("GET", ["saved-snapshots", name]) => {
                let generation = world.generations.get(*name).copied().unwrap_or(1);
                if let Some(left) = world.stale.get_mut(*name).filter(|left| **left > 0) {
                    *left -= 1;
                    return Reply::json(
                        200,
                        &json!({"ok": true,
                                "snapshot": snapshot_json(name, "ready", generation - 1)})
                        .to_string(),
                    );
                }
                match world.snapshots.get_mut(*name) {
                    Some((status, polls)) => {
                        let shown = if *polls > 0 {
                            *polls -= 1;
                            "saving".to_string()
                        } else {
                            status.clone()
                        };
                        Reply::json(
                            200,
                            &json!({"ok": true,
                                    "snapshot": snapshot_json(name, &shown, generation)})
                            .to_string(),
                        )
                    }
                    None => not_found(),
                }
            }
            ("DELETE", ["saved-snapshots", name]) => match world.snapshots.remove(*name) {
                Some(_) => Reply::json(200, r#"{"ok":true,"status":"removed"}"#),
                None => not_found(),
            },
            (method, ["sandboxes", id, rest @ ..]) => {
                let hydrate_never = world.hydrate_never;
                let Some(sandbox) = world.sandboxes.get_mut(*id).filter(|s| !s.deleted) else {
                    return not_found();
                };
                let hydrated = !(hydrate_never && sandbox.role == "builder");
                match (method, rest) {
                    ("GET" | "PATCH", []) => Reply::json(
                        200,
                        &json!({"ok": true, "sandbox": sandbox_json(id, sandbox, hydrated)})
                            .to_string(),
                    ),
                    ("DELETE", []) => {
                        sandbox.deleted = true;
                        Reply::json(202, &operation(id))
                    }
                    ("POST", ["stop"]) => {
                        sandbox.state = "archived";
                        Reply::json(202, r#"{"ok":true,"status":"archiving"}"#)
                    }
                    ("POST", ["resume"]) => {
                        sandbox.state = "idle";
                        Reply::json(
                            200,
                            &json!({"ok": true, "status": "running",
                                    "sandbox": sandbox_json(id, sandbox, hydrated)})
                            .to_string(),
                        )
                    }
                    ("POST", ["sshkey"]) => Reply::json(
                        200,
                        &json!({"ok": true, "success": true, "machineIp": "198.51.100.7",
                                "sshUser": "user", "sshEndpoint": "203.0.113.10:22001",
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

fn world() -> Shared {
    Arc::new(Mutex::new(World {
        save_ends: "ready".to_string(),
        ..World::default()
    }))
}

/// A project whose base ref (origin/main) holds a large incompressible file,
/// and whose base build writes into an ignored directory.
fn project(label: &str) -> E2e {
    let setup = E2e::with_history(label, None);
    let mut text = fs::read_to_string(setup.repo().join("boatparty.toml")).unwrap();
    text.push_str(
        "\n[base]\nwarm = \"mkdir -p ignored && echo warmed > ignored/warm.out\"\n\
         stale_inputs = [\"deps.lock\"]\n",
    );
    fs::write(setup.repo().join("boatparty.toml"), text).unwrap();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let noise: Vec<u8> = (0..300_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    fs::write(setup.repo().join("noise.bin"), noise).unwrap();
    fs::write(setup.repo().join("deps.lock"), "v1\n").unwrap();
    git(&setup.repo(), &["add", "-A"]);
    git(&setup.repo(), &["commit", "-qm", "dependencies"]);
    git(
        &setup.repo(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    setup
}

fn json_of(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is one JSON document")
}

fn state(setup: &E2e) -> Value {
    serde_json::from_str(&fs::read_to_string(setup.home().join("state/state.json")).unwrap())
        .unwrap()
}

fn position(lines: &[String], from: usize, wanted: &str) -> usize {
    from + lines[from..]
        .iter()
        .position(|line| line.starts_with(wanted))
        .unwrap_or_else(|| panic!("no {wanted:?} after line {from}: {lines:#?}"))
}

#[test]
fn a_build_runs_its_steps_in_order_and_records_the_base() {
    let setup = project("base-order");
    let world = world();
    let boat = boat(&setup, Arc::clone(&world));
    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "build"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let report = json_of(&output.stdout);
    assert_shape("base build", &report);
    let head = git(&setup.repo(), &["rev-parse", "HEAD"]);
    assert_eq!(report["base"]["snapshot"], "bp-app");
    assert_eq!(report["base"]["commit"], head);
    assert_eq!(report["base"]["builder_id"], "bx_b");
    assert_eq!(report["base"]["ref"], "refs/remotes/origin/main");
    assert_eq!(report["base"]["branch"], "main");
    assert_eq!(report["base"]["mode"], "head");
    assert_eq!(report["builder"]["created"], true);

    // The sequence: create the builder (blank, no secrets), run the steps
    // and the warm command, settle, look at hydration, stop, wait for the
    // stop, save, wait until the save is ready.
    let lines = setup.event_lines();
    let create = position(&lines, 0, "provider POST /sandboxes ");
    assert!(
        lines[create].contains("\"noEnv\":true"),
        "{}",
        lines[create]
    );
    assert!(lines[create].contains("\"boatparty.role\":\"builder\""));
    assert!(lines[create].contains("\"name\":\"bp-app-builder\""));
    assert!(!lines[create].contains("\"from\""));
    let run = position(&lines, create, "ssh-run");
    let settle = position(&lines, run, "ssh-settle");
    let hydration = position(&lines, settle, "provider GET /sandboxes/bx_b");
    let stop = position(&lines, hydration, "provider POST /sandboxes/bx_b/stop");
    let stopped = position(&lines, stop, "provider GET /sandboxes/bx_b");
    let save = position(&lines, stopped, "provider POST /saved-snapshots");
    assert!(lines[save].contains("\"sandboxId\":\"bx_b\""));
    assert!(lines[save].contains("\"name\":\"bp-app\""));
    position(&lines, save, "provider GET /saved-snapshots/bp-app");
    assert!(
        !lines[..stop]
            .iter()
            .any(|line| line.contains("/saved-snapshots")),
        "saved before the stop: {lines:#?}"
    );

    // The record, and the builder kept (stopped) for the next build.
    let state = state(&setup);
    let base = &state["projects"]["app"]["base"];
    assert_eq!(base["snapshot"], "bp-app");
    assert_eq!(base["commit"], head);
    assert!(base["known"].as_array().unwrap().contains(&json!(head)));
    assert!(base["refs_digest"].is_string());
    assert!(base["built_at"].as_u64().unwrap() > 0);
    let members = state["projects"]["app"]["members"].as_array().unwrap();
    let builder = members.iter().find(|m| m["role"] == "builder").unwrap();
    assert_eq!(builder["name"], "bp-app-builder");
    assert!(builder["leases"].as_array().unwrap().is_empty());
    assert_eq!(world.lock().unwrap().sandboxes["bx_b"].state, "archived");
    // The warm command ran in the builder's checkout; settling left no
    // per-invocation scratch behind.
    let checkout = setup.remote().join("app");
    assert_eq!(
        fs::read_to_string(checkout.join("ignored/warm.out")).unwrap(),
        "warmed\n"
    );
    let out = setup.remote().join(".boatparty/out");
    assert_eq!(fs::read_dir(&out).map(|dir| dir.count()).unwrap_or(0), 0);

    // A rebuild resumes the same builder, which is never counted as a pool
    // member.
    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "build"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json_of(&output.stdout);
    assert_eq!(report["builder"]["created"], false);
    assert_eq!(report["builder"]["resumed"], true);
    assert_eq!(world.lock().unwrap().created, 1);
}

#[test]
fn a_build_whose_hydration_never_completes_does_not_stop_the_builder() {
    let setup = project("base-hydration");
    let world = world();
    world.lock().unwrap().hydrate_never = true;
    let boat = boat(&setup, Arc::clone(&world));
    let output = setup
        .cmd(&boat)
        .env("BOATPARTY_HYDRATION_TIMEOUT_SECONDS", "1")
        .args(["--json", "base", "build"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let report = json_of(&output.stdout);
    assert_eq!(report["error"]["code"], "hydration_timeout");
    let lines = setup.event_lines();
    assert!(lines.iter().any(|line| line == "ssh-settle"));
    assert!(
        !lines.iter().any(|line| line.contains("/stop")),
        "stopped while hydrating: {lines:#?}"
    );
    assert!(!lines.iter().any(|line| line.contains("/saved-snapshots")));
    assert!(state(&setup)["projects"]["app"]["base"].is_null());
    // Let the idle helper this left behind see a complete builder and end.
    world.lock().unwrap().hydrate_never = false;
}

#[test]
fn a_failed_build_leaves_the_previous_base_record_and_snapshot_intact() {
    let setup = project("base-failed");
    let world = world();
    let boat = boat(&setup, Arc::clone(&world));
    setup.cmd(&boat).args(["base", "build"]).assert().success();
    let before = state(&setup)["projects"]["app"]["base"].clone();
    assert!(before.is_object());

    // A save that fails.
    world.lock().unwrap().save_ends = "failed".to_string();
    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "build"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(json_of(&output.stdout)["error"]["code"], "base_save_failed");
    assert_eq!(state(&setup)["projects"]["app"]["base"], before);

    // A warm command that fails.
    let file = setup.repo().join("boatparty.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("echo warmed > ignored/warm.out", "exit 3"),
    )
    .unwrap();
    world.lock().unwrap().save_ends = "ready".to_string();
    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "build"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        json_of(&output.stdout)["error"]["code"],
        "base_build_failed"
    );
    assert_eq!(state(&setup)["projects"]["app"]["base"], before);
    let lines = setup.event_lines();
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("provider DELETE /saved-snapshots")),
        "{lines:#?}"
    );
    assert!(world.lock().unwrap().snapshots.contains_key("bp-app"));
}

#[test]
fn members_start_from_the_base_with_its_sync_knowledge_and_sync_incrementally() {
    let setup = project("base-member");
    let world = world();
    let boat = boat(&setup, Arc::clone(&world));

    // Without a base: a blank create, and no question about a base.
    let output = setup.cmd(&boat).args(["--json", "sync"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let blank_pack = json_of(&output.stdout)["pack_bytes"].as_u64().unwrap();
    assert!(
        blank_pack > 250_000,
        "a first sync sends everything: {blank_pack}"
    );
    let lines = setup.event_lines();
    let create = position(&lines, 0, "provider POST /sandboxes ");
    assert!(!lines[create].contains("\"from\""), "{}", lines[create]);
    assert!(lines[create].contains("\"boatparty.role\":\"pool\""));
    assert!(!lines.iter().any(|line| line.contains("saved-snapshots")));
    let first = state(&setup)["projects"]["app"]["members"][0].clone();

    // A new base leaves the existing member as it was.
    setup.cmd(&boat).args(["base", "build"]).assert().success();
    let after = state(&setup);
    let member = after["projects"]["app"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "pool")
        .unwrap()
        .clone();
    assert_eq!(member["sync"], first["sync"]);
    assert_eq!(member["id"], first["id"]);

    // A new member starts from the base, knowing what the base holds.
    setup.cmd(&boat).args(["rm"]).assert().success();
    let mark = setup.event_lines().len();
    setup.start(&boat);
    let lines = setup.event_lines();
    let create = position(&lines, mark, "provider POST /sandboxes ");
    assert!(
        lines[create].contains("\"from\":\"bp-app\""),
        "{}",
        lines[create]
    );
    assert!(lines[create].contains("\"noEnv\":true"));
    let state = state(&setup);
    let base = state["projects"]["app"]["base"].clone();
    let member = state["projects"]["app"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "pool")
        .unwrap()
        .clone();
    assert_eq!(member["sync"]["known"], base["known"]);
    assert_eq!(member["sync"]["refs_digest"], base["refs_digest"]);
    assert_eq!(member["sync"]["mode"], "head");

    // Its first sync sends only what changed since the base commit.
    fs::write(setup.repo().join("a.txt"), "changed\n").unwrap();
    let output = setup.cmd(&boat).args(["--json", "sync"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json_of(&output.stdout);
    assert_eq!(report["full_retry"], false, "{report}");
    let pack = report["pack_bytes"].as_u64().unwrap();
    assert!(
        pack < 10_000,
        "the first sync after the base sent {pack} bytes"
    );
    assert_eq!(report["seed_route"], "none");
}

#[test]
fn base_status_reports_freshness_and_rm_removes_snapshot_record_and_builder() {
    let setup = project("base-status");
    let world = world();
    let boat = boat(&setup, Arc::clone(&world));

    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report = json_of(&output.stdout);
    assert!(report["base"].is_null());
    assert_eq!(report["allowance"]["included"], 10);

    setup.cmd(&boat).args(["base", "build"]).assert().success();
    // Two commits on the base ref, one of them changing a stale input.
    fs::write(setup.repo().join("deps.lock"), "v2\n").unwrap();
    git(&setup.repo(), &["commit", "-qam", "bump"]);
    fs::write(setup.repo().join("a.txt"), "next\n").unwrap();
    git(&setup.repo(), &["commit", "-qam", "next"]);
    git(
        &setup.repo(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "status"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json_of(&output.stdout);
    assert_shape("base status", &report);
    assert_eq!(report["saved"]["status"], "ready");
    assert_eq!(report["staleness"]["behind"], 2);
    assert_eq!(report["staleness"]["inputs_changed"], json!(["deps.lock"]));
    assert_eq!(report["stale"], true);
    assert_eq!(report["builder"]["name"], "bp-app-builder");
    // status says so too.
    let output = setup
        .cmd(&boat)
        .args(["--json", "status"])
        .output()
        .unwrap();
    let status = json_of(&output.stdout);
    assert_shape("status", &status);
    assert_eq!(status["base"]["present"], true);
    assert_eq!(status["base"]["stale"], true);

    let output = setup
        .cmd(&boat)
        .args(["--json", "base", "rm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json_of(&output.stdout);
    assert_shape("base rm", &report);
    assert_eq!(report["snapshot_deleted"], true);
    assert_eq!(report["record_removed"], true);
    assert_eq!(report["builder_deleted"]["id"], "bx_b");
    assert!(world.lock().unwrap().snapshots.is_empty());
    assert!(world.lock().unwrap().sandboxes["bx_b"].deleted);
    let state = state(&setup);
    assert!(state["projects"]["app"]["base"].is_null());
    assert!(
        !state["projects"]["app"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "builder")
    );
}

#[test]
fn a_rebuild_waits_for_the_new_artifact_not_a_stale_ready() {
    let setup = project("base-rebuild");
    let world = world();
    let boat = boat(&setup, Arc::clone(&world));
    let build = || {
        let output = setup
            .cmd(&boat)
            .args(["--json", "base", "build"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        json_of(&output.stdout)
    };
    build();
    // The base ref moves; the rebuild's save is answered with the old
    // artifact as ready a few times before the new save shows.
    fs::write(setup.repo().join("deps.lock"), "v2\n").unwrap();
    git(&setup.repo(), &["commit", "-qam", "bump"]);
    git(
        &setup.repo(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    world.lock().unwrap().stale_polls = 3;
    let report = build();
    assert_eq!(
        report["base"]["commit"],
        git(&setup.repo(), &["rev-parse", "HEAD"])
    );
    let lines = setup.event_lines();
    let second_save = lines
        .iter()
        .rposition(|line| line.starts_with("provider POST /saved-snapshots"))
        .unwrap();
    let polls = lines[second_save..]
        .iter()
        .filter(|line| line.starts_with("provider GET /saved-snapshots/bp-app"))
        .count();
    // Three stale answers, one `saving`, then the new artifact's `ready`.
    assert_eq!(polls, 5, "{lines:#?}");
    assert!(world.lock().unwrap().stale["bp-app"] == 0);
}
