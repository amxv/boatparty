//! `fetch`, the default `head` history and signed uploads end to end, with a
//! fake `ssh` that runs remote commands locally and a fake boat API.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::e2e::{E2e, ID, URL_SECRET};
use common::runner::{git, write};
use serde_json::Value;

fn json_doc(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(bytes)))
}

/// About 40 KB of text that does not compress much.
fn noise(seed: u64) -> String {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..2500)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            format!("{state:016x}\n")
        })
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn fetch_copies_files_and_directories_by_relative_or_absolute_path() {
    let setup = E2e::new("fetch-paths");
    let boat = setup.boat();
    setup.cmd(&boat).arg("launch").assert().success();
    let checkout = setup.remote().join("app");
    write(
        &checkout.join("target/release/app"),
        "#!/bin/sh\necho binary\n",
    );
    fs::set_permissions(
        checkout.join("target/release/app"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write(&checkout.join("target/doc/index.html"), "<html>\n");
    write(&checkout.join("target/doc/sub/page.html"), "<p>\n");
    write(&setup.remote().join("elsewhere/report.txt"), "report\n");
    let out = setup.root.path().join("out");
    fs::create_dir_all(&out).unwrap();

    // A relative file into an existing directory keeps its name.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "fetch", "target/release/app"])
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["remote_path"], "/home/user/app/target/release/app");
    assert_eq!(doc["kind"], "file");
    assert_eq!(doc["files"], 1);
    assert_eq!(doc["bytes"], 22);
    assert_eq!(doc["member"]["id"], ID);
    assert_eq!(doc["route"], "ssh", "a small archive comes back over SSH");
    assert_eq!(doc["download_fallback"], Value::Null);
    let fetched = out.join("app");
    assert_eq!(
        doc["local_path"],
        fs::canonicalize(&fetched).unwrap().display().to_string()
    );
    assert_eq!(
        fs::read_to_string(&fetched).unwrap(),
        "#!/bin/sh\necho binary\n"
    );
    assert_ne!(
        fs::metadata(&fetched).unwrap().permissions().mode() & 0o111,
        0,
        "the executable bit survives"
    );

    // Again: an existing file is replaced.
    write(&checkout.join("target/release/app"), "#!/bin/sh\necho v2\n");
    setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["fetch", "target/release/app"])
        .arg(&out)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(&fetched).unwrap(),
        "#!/bin/sh\necho v2\n"
    );

    // A directory under a new name.
    let docs = out.join("docs-copy");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "fetch", "target/doc/"])
        .arg(&docs)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["kind"], "directory");
    assert_eq!(doc["files"], 2);
    assert_eq!(
        fs::read_to_string(docs.join("sub/page.html")).unwrap(),
        "<p>\n"
    );
    // An existing directory receives it inside, under its own name.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "fetch", "target/doc"])
        .arg(&docs)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(docs.join("doc/index.html").exists());
    // An existing directory is never overwritten.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "fetch", "target/doc"])
        .arg(&docs)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(json_doc(&output.stdout)["error"]["code"], "fetch_failed");

    // An absolute path, into the current directory by default.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["fetch", "/home/user/elsewhere/report.txt"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        fs::read_to_string(setup.repo().join("report.txt")).unwrap(),
        "report\n"
    );
    assert!(
        text(&output.stdout).contains("fetched /home/user/elsewhere/report.txt"),
        "{}",
        text(&output.stdout)
    );

    // A missing path is a boatparty failure with its own message.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "fetch", "target/nothing-here"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["error"]["code"], "fetch_failed");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/home/user/app/target/nothing-here"),
        "{doc}"
    );

    // No staging directory is left behind, and no lease.
    let leftovers: Vec<String> = fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".boatparty-fetch-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let state: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("state/state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["projects"]["app"]["members"][0]["leases"],
        serde_json::json!([])
    );
}

/// The archives `fetch` leaves on the sandbox.
fn leftover_archives(setup: &E2e) -> Vec<String> {
    fs::read_dir(setup.remote().join(".boatparty/out"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("fetch-"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_large_fetch_comes_back_by_signed_download_and_falls_back_to_ssh() {
    let setup = E2e::new("fetch-signed");
    let boat = setup.boat();
    setup.cmd(&boat).arg("launch").assert().success();
    let payload = noise(3);
    write(&setup.remote().join("app/target/big.txt"), &payload);
    let dest = setup.root.path().join("big-copy.txt");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SIGNED_URL_MIN_BYTES", "0")
        .args(["--json", "fetch", "target/big.txt"])
        .arg(&dest)
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["route"], "signed_url");
    assert!(doc["archive_bytes"].as_u64().unwrap() > 0);
    assert_eq!(fs::read_to_string(&dest).unwrap(), payload);
    assert!(!text(&output.stdout).contains(URL_SECRET));
    assert!(!stderr.contains(URL_SECRET), "{stderr}");
    assert!(
        leftover_archives(&setup).is_empty(),
        "{:?}",
        leftover_archives(&setup)
    );
    assert!(
        setup
            .event_lines()
            .iter()
            .any(|line| line.starts_with("provider GET /download/")),
        "{:#?}",
        setup.event_lines()
    );

    // A refused download falls back to SSH and says why, without the URL.
    let refusing = setup.boat_with_uploads(403);
    let again = setup.root.path().join("big-again.txt");
    let output = setup
        .cmd(&refusing)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SIGNED_URL_MIN_BYTES", "0")
        .args(["--json", "fetch", "target/big.txt"])
        .arg(&again)
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["route"], "ssh");
    let reason = doc["download_fallback"]
        .as_str()
        .expect("a fallback reason");
    assert!(reason.contains("403"), "{reason}");
    assert!(!text(&output.stdout).contains(URL_SECRET));
    assert!(!stderr.contains(URL_SECRET), "{stderr}");
    assert_eq!(fs::read_to_string(&again).unwrap(), payload);
    assert!(leftover_archives(&setup).is_empty());
}

#[test]
fn the_default_history_is_head_and_a_large_pack_travels_by_signed_url_without_leaking_it() {
    let setup = E2e::with_history("upload-e2e", None);
    let boat = setup.boat();
    setup.cmd(&boat).arg("launch").assert().success();
    for n in 0..3 {
        write(&setup.repo().join("history.txt"), &format!("{n}\n"));
        git(&setup.repo(), &["add", "-A"]);
        git(&setup.repo(), &["commit", "-qm", &format!("more {n}")]);
    }
    write(&setup.repo().join("payload.txt"), &noise(1));
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SIGNED_URL_MIN_BYTES", "0")
        .args(["--json", "load"])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["history"], "head");
    assert_eq!(doc["pack_route"], "signed_url");
    assert_eq!(doc["upload_fallback"], Value::Null);
    assert!(!text(&output.stdout).contains(URL_SECRET));
    assert!(!stderr.contains(URL_SECRET), "{stderr}");
    let remote = setup.remote().join("app");
    assert_eq!(git(&remote, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git(&remote, &["rev-parse", "HEAD"]),
        git(&setup.repo(), &["rev-parse", "HEAD"])
    );
    assert_eq!(
        fs::read_to_string(remote.join("payload.txt")).unwrap(),
        noise(1)
    );
    let events = setup.event_lines();
    assert!(
        events.iter().any(
            |line| line.starts_with(&format!("provider POST /sandboxes/{ID}/files/upload-url"))
        ),
        "{events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|line| line.starts_with("provider PUT /upload/"))
    );

    // A run with the same threshold, so runs.jsonl sees an uploaded pack too.
    write(&setup.repo().join("payload.txt"), &noise(2));
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SIGNED_URL_MIN_BYTES", "0")
        .args(["cheers", "--", "git", "log", "--oneline"])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        text(&output.stdout).lines().count(),
        1,
        "{}",
        text(&output.stdout)
    );
    assert!(stderr.contains("pack sent by signed upload"), "{stderr}");
    assert!(!stderr.contains(URL_SECRET), "{stderr}");
    let state_dir = setup.home().join("state");
    let state = fs::read_to_string(state_dir.join("state.json")).unwrap();
    let runs = fs::read_to_string(state_dir.join("runs.jsonl")).unwrap();
    assert!(!state.contains(URL_SECRET));
    assert!(!runs.contains(URL_SECRET));
    assert!(runs.contains("\"pack_route\":\"signed_url\""), "{runs}");
    let state: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        state["projects"]["app"]["members"][0]["sync"]["mode"],
        "head"
    );
}

#[test]
fn a_refused_signed_upload_falls_back_to_ssh_and_reports_no_url() {
    let setup = E2e::new("upload-refused");
    let boat = setup.boat_with_uploads(500);
    setup.cmd(&boat).arg("launch").assert().success();
    write(&setup.repo().join("payload.txt"), &noise(1));
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SIGNED_URL_MIN_BYTES", "1024")
        .args(["--json", "load"])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let doc = json_doc(&output.stdout);
    assert_eq!(doc["history"], "full");
    assert_eq!(doc["pack_route"], "ssh");
    let reason = doc["upload_fallback"].as_str().expect("a fallback reason");
    assert!(reason.contains("500"), "{reason}");
    assert!(!text(&output.stdout).contains(URL_SECRET));
    assert!(!stderr.contains(URL_SECRET), "{stderr}");
    assert_eq!(
        fs::read_to_string(setup.remote().join("app/payload.txt")).unwrap(),
        noise(1)
    );

    // Small packs never ask the provider for anything.
    let _ = fs::remove_file(setup.events());
    write(&setup.repo().join("small.txt"), "small\n");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(json_doc(&output.stdout)["pack_route"], "ssh");
    assert!(
        !setup
            .event_lines()
            .iter()
            .any(|line| line.starts_with("provider")),
        "{:#?}",
        setup.event_lines()
    );
}
