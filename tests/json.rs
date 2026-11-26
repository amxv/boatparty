//! Every subcommand's `--json` output against `docs/json.md`, the contract.
//!
//! The commands run end to end against the fake boat API and the fake
//! `ssh` of `common::e2e`. Each output is compared with the example the
//! document gives for its shape (`<!-- shape: NAME -->` before a ```json
//! block): the top-level fields must be exactly the documented ones, and
//! nested objects may hold only documented fields (an object with a `"*"`
//! key in the document is a map with free keys). Every subcommand is also
//! run where boatparty must fail, and must then print the error shape and
//! exit 125.

#![cfg(unix)] // These fixtures execute the Linux runner or POSIX tool shims locally.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::contract::{conform, documented};
use common::e2e::{E2e, ID};
use common::{FakeBoat, Reply, TEST_KEY, boatparty, fixture};
use serde_json::{Value, json};

/// What one run printed, as the document to check: the one JSON document,
/// or for a streaming command the result frame (after checking that every
/// line is a frame).
fn parse(shape: &str, stdout: &[u8], streaming: bool) -> Value {
    let text = String::from_utf8_lossy(stdout);
    if !streaming {
        return serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{shape}: not one JSON document ({error}):\n{text}"));
    }
    let frames: Vec<Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{shape}: not a frame ({error}): {line}"))
        })
        .collect();
    for frame in &frames {
        let kind = frame["type"].as_str().unwrap_or_default();
        assert!(
            ["stdout", "stderr", "event", "result"].contains(&kind),
            "{shape}: unknown frame {frame}"
        );
    }
    let last = frames
        .last()
        .unwrap_or_else(|| panic!("{shape}: no frames"));
    assert_eq!(
        last["type"], "result",
        "{shape}: the last frame is not the result"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["type"] == "result")
            .count(),
        1,
        "{shape}: more than one result frame"
    );
    last.clone()
}

/// Shapes whose success another test checks, where the fakes can produce
/// it: a whole base build needs `tests/base.rs`'s provider.
const CHECKED_ELSEWHERE: &[&str] = &[
    "base build",
    "url",
    "gate",
    "secret set",
    "secret rm",
    "secret list",
    "env status",
    "env apply",
];

/// Collected outputs and how they compare.
#[derive(Default)]
struct Seen {
    docs: Vec<(String, Value)>,
}

impl Seen {
    fn add(&mut self, shape: &str, output: &std::process::Output, streaming: bool) {
        let document = parse(shape, &output.stdout, streaming);
        let ok = document["ok"].as_bool();
        let code = output.status.code();
        match ok {
            Some(true) => {}
            Some(false) => assert_eq!(
                code,
                Some(125),
                "{shape}: a failure exits 125: {document}\n{}",
                String::from_utf8_lossy(&output.stderr)
            ),
            None => panic!("{shape}: no ok field: {document}"),
        }
        self.docs.push((shape.to_string(), document));
    }

    /// Compare everything seen with the document. Every documented shape
    /// must have been seen.
    fn check(&self) {
        let shapes = documented();
        let mut errors = Vec::new();
        for (shape, document) in &self.docs {
            match shapes.get(shape) {
                Some(doc) => conform(doc, document, shape, true, &mut errors),
                None => errors.push(format!("{shape} is not documented")),
            }
        }
        for name in shapes.keys() {
            if !CHECKED_ELSEWHERE.contains(&name.as_str())
                && !self.docs.iter().any(|(shape, _)| shape == name)
            {
                errors.push(format!("{name} is documented but no command printed it"));
            }
        }
        if !errors.is_empty() {
            for (shape, document) in &self.docs {
                eprintln!(
                    "--- {shape}\n{}",
                    serde_json::to_string_pretty(document).unwrap()
                );
            }
            panic!("docs/json.md and the output differ:\n{}", errors.join("\n"));
        }
    }
}

/// A sandbox as the fake reports it.
fn sandbox(state: &str) -> Value {
    json!({
        "id": ID, "name": "bp-app-1", "state": state, "hydrated": true,
        "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
        "type": "default", "archiveAfter": "2026-11-15T12:15:00Z",
    })
}

/// The fake boat API for the whole tour: one pool sandbox that can be
/// stopped and deleted, limits, hosting, an empty snapshot list, and a 400
/// for doctor's create probe.
fn world() -> FakeBoat {
    let state = Arc::new(Mutex::new("idle".to_string()));
    let desktop_calls = std::sync::atomic::AtomicUsize::new(0);
    let limits = fixture("limits.json");
    let invalid = fixture("errors/400-invalid-type.json");
    FakeBoat::start(move |request| {
        let mut current = state.lock().unwrap();
        let operation = |status: &str| {
            json!({"ok": true, "type": "deletion.operation", "operation": {
                "id": "bdop_1", "kind": "sandbox", "targetId": ID, "reason": "explicit",
                "status": status, "attemptCount": 1, "requestedAt": "2026-11-15T00:00:00Z",
                "completedAt": null}})
            .to_string()
        };
        let path = request.path.split('?').next().unwrap_or_default();
        match (request.method.as_str(), path) {
            ("GET", "/limits") => Reply::json(200, &limits),
            ("POST", "/sandboxes") if request.body.contains("scope-probe") => {
                Reply::json(400, &invalid)
            }
            ("POST", "/sandboxes") => {
                *current = "idle".to_string();
                Reply::json(
                    202,
                    &json!({"ok": true, "type": "sandbox.created", "status": "provisioning",
                            "ttlSeconds": 300, "sandbox": sandbox("idle")})
                    .to_string(),
                )
            }
            ("GET", "/sandboxes") => {
                let listed: Vec<Value> = if *current == "deleted" {
                    Vec::new()
                } else {
                    vec![sandbox(&current)]
                };
                Reply::json(
                    200,
                    &json!({"ok": true, "sandboxes": listed,
                            "pageInfo": {"nextCursor": null, "hasMore": false}})
                    .to_string(),
                )
            }
            ("POST", p) if p == format!("/sandboxes/{ID}/sshkey") => Reply::json(
                200,
                &json!({"ok": true, "type": "ssh_key.configured", "success": true,
                        "machineIp": "198.51.100.7", "sshUser": "user",
                        "sshEndpoint": "203.0.113.10:22001",
                        "hostKey": "ssh-ed25519 AAAAhostkey"})
                .to_string(),
            ),
            ("POST", p)
                if p == format!("/sandboxes/{ID}/desktop")
                    && desktop_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 =>
            {
                Reply::json(409, r#"{"code":"desktop_not_ready"}"#)
            }
            ("POST", p) if p == format!("/sandboxes/{ID}/desktop") => Reply::json(
                200,
                r#"{"desktopUrl":"https://example.invalid/?token=test-private"}"#,
            ),
            ("POST", p) if p == format!("/sandboxes/{ID}/host") => Reply::json(
                200,
                &json!({"ok": true, "type": "port.hosted", "success": true, "port": 8000,
                        "url": "https://app-8000.on.boat.example?_token=t", "isProtected": true,
                        "access": "private", "localhost": false})
                .to_string(),
            ),
            ("POST", p) if p == format!("/sandboxes/{ID}/stop") => {
                *current = "archived".to_string();
                Reply::json(
                    202,
                    &json!({"ok": true, "type": "sandbox.stopping", "id": ID,
                            "status": "archiving"})
                    .to_string(),
                )
            }
            ("DELETE", p) if p == format!("/sandboxes/{ID}") => {
                *current = "deleted".to_string();
                Reply::json(202, &operation("processing"))
            }
            ("GET", p) if p.starts_with("/deletion-operations/") => {
                Reply::json(200, &operation("completed"))
            }
            ("GET" | "PATCH", p) if p == format!("/sandboxes/{ID}") && *current != "deleted" => {
                Reply::json(
                    200,
                    &json!({"ok": true, "type": "sandbox.info", "sandbox": sandbox(&current)})
                        .to_string(),
                )
            }
            ("GET", "/saved-snapshots") => Reply::json(
                200,
                &json!({"ok": true, "snapshots": [], "allowance": {
                    "included": 10, "used": 0, "extra": 0,
                    "extraMonthlyDollars": 0.0, "pricePerExtraDailyDollars": 0.056}})
                .to_string(),
            ),
            _ => Reply::json(404, r#"{"ok":false,"code":"not_found","status":404}"#),
        }
    })
}

/// Run one command with `--json` and record its document as `shape`.
fn run(
    seen: &mut Seen,
    setup: &E2e,
    boat: &FakeBoat,
    shape: &str,
    args: &[&str],
    stdin: &[u8],
) -> Value {
    let streaming = ["run", "gate", "serve", "shell", "pipe"].contains(&args[0]);
    let output = setup
        .cmd(boat)
        .env("BOATPARTY_RUNNER_SYSTEMCTL", "true")
        .arg("--json")
        .args(args)
        .write_stdin(stdin.to_vec())
        .output()
        .unwrap();
    if shape == "desktop" {
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("test-private"));
        for file in ["state.json", "runs.jsonl"] {
            assert!(
                !fs::read_to_string(setup.home().join("state").join(file))
                    .unwrap_or_default()
                    .contains("test-private")
            );
        }
    }
    seen.add(shape, &output, streaming);
    seen.docs.last().unwrap().1.clone()
}

/// `forward` holds until a signal: wait for its one document, then stop it.
fn forward(seen: &mut Seen, setup: &E2e, boat: &FakeBoat) {
    let mut child = setup
        .spawn_cmd(boat)
        .args(["--json", "forward", "18000:18765"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            text.push(line.clone());
            if line == "}" {
                let _ = tx.send(text.join("\n"));
                text.clear();
            }
        }
    });
    let document = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("forward printed its document");
    std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(143));
    let value: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(value["ok"], true);
    seen.docs.push(("forward".to_string(), value));
}

#[test]
fn every_subcommand_prints_its_documented_json() {
    let setup = E2e::with_history("json", None);
    let host = setup.root.path().join("bin/host");
    fs::write(&host, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
    let boat = world();
    let mut seen = Seen::default();
    let mut go = |shape: &str, args: &[&str]| run(&mut seen, &setup, &boat, shape, args, b"");

    go("skill", &["skill"]);
    go("skill --print", &["skill", "--print"]);
    go("init", &["init"]);
    go("config show", &["config", "show"]);
    go("config show --resolved", &["config", "show", "--resolved"]);
    go("config show --origin", &["config", "show", "--origin"]);
    go("config validate", &["config", "validate"]);
    go("preset list", &["preset", "list"]);
    go("preset show", &["preset", "show", "rust"]);
    go("preset eject", &["preset", "eject", "go"]);
    go("limits", &["limits"]);
    go("limits starts", &["limits", "starts"]);
    go(
        "limits set",
        &["limits", "set", "project", "app", "max_sandboxes=3"],
    );
    go("start", &["start"]);
    go("status", &["status"]);
    go("sync", &["sync"]);
    go("run", &["run", "--", "sh", "-c", "echo out; echo err >&2"]);
    go("serve", &["serve", "--", "true"]);
    go("shell", &["shell", "--", "echo", "hi"]);
    go(
        "fetch",
        &[
            "fetch",
            "a.txt",
            setup.root.path().join("fetched.txt").to_str().unwrap(),
        ],
    );
    go("desktop", &["desktop"]);
    go("expose", &["expose", "8000"]);
    go("expose --hide", &["expose", "8000", "--hide"]);
    go("release", &["release"]);
    go("assign", &["assign", "bp-app-1"]);
    go("log", &["log"]);
    go("pool", &["pool"]);
    go("pool", &["pool", "--refresh"]);
    go("pool prune", &["pool", "prune", "--yes"]);
    go("base status", &["base", "status"]);
    go("stop", &["stop"]);
    go("base rm", &["base", "rm"]);
    go("rm", &["rm"]);
    go("doctor", &["doctor"]);
    let piped = run(
        &mut seen,
        &setup,
        &boat,
        "pipe",
        &["pipe", "--", "cat"],
        b"piped input\n",
    );
    assert_eq!(piped["input_bytes"], 12);
    run(
        &mut seen,
        &setup,
        &boat,
        "login",
        &["login", "--key-stdin", "--no-verify"],
        format!("{TEST_KEY}\n").as_bytes(),
    );
    forward(&mut seen, &setup, &boat);

    // A failure of boatparty itself: a base build whose sandbox the
    // provider refuses (billing).
    let refused = E2e::with_history("json-refused", None);
    let billing = fixture("errors/402-billing-required.json");
    let no = FakeBoat::start(move |_| Reply::json(402, &billing));
    run(&mut seen, &refused, &no, "error", &["base", "build"], b"");
    run(
        &mut seen,
        &refused,
        &no,
        "error frame",
        &["run", "--", "true"],
        b"",
    );
    seen.check();
}

/// Every subcommand, where boatparty cannot even find its files: the error
/// document (a result frame for streaming commands) and exit 125.
#[test]
fn every_subcommand_fails_with_the_error_shape_and_exit_125() {
    let cases: &[&[&str]] = &[
        &["start"],
        &["url"],
        &["sync"],
        &["run", "--", "true"],
        &["serve", "--", "true"],
        &["shell", "--", "true"],
        &["stop"],
        &["pipe", "--", "cat"],
        &["fetch", "a.txt"],
        &["forward", "8000"],
        &["desktop"],
        &["expose", "8000"],
        &["expose", "8000", "--hide"],
        &["init"],
        &["status"],
        &["pool"],
        &["pool", "prune", "--yes"],
        &["config", "show"],
        &["config", "validate"],
        &["preset", "list"],
        &["preset", "show", "rust"],
        &["preset", "eject", "rust"],
        &["release"],
        &["assign", "bp-app-1"],
        &["rm"],
        &["gate", "--", "true"],
        &["base", "build"],
        &["base", "status"],
        &["base", "rm"],
        &["login", "--key-stdin"],
        &["doctor"],
        &["limits"],
        &["limits", "starts"],
        &["limits", "set", "account", "max_running=2"],
        &["log"],
        &["skill"],
        &["skill", "--print"],
    ];
    let shapes = documented();
    let started = Instant::now();
    for args in cases {
        let streaming = ["run", "gate", "serve", "shell", "pipe"].contains(&args[0]);
        let output = boatparty(Path::new("/"))
            .env("BOATPARTY_HOME", "relative/home")
            .arg("--json")
            .args(*args)
            .write_stdin("")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(125), "{args:?}: {output:?}");
        let shape = if streaming { "error frame" } else { "error" };
        let document = parse(shape, &output.stdout, streaming);
        let mut errors = Vec::new();
        conform(&shapes[shape], &document, shape, true, &mut errors);
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{document}");
        assert_eq!(document["error"]["code"], "config", "{args:?}");
    }
    assert!(started.elapsed() < Duration::from_secs(60));
}
