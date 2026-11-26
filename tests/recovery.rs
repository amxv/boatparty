//! Recovery concurrency and tunnel repair through CLI entry points.
#![cfg(unix)]
mod common;

use std::fs;

use common::e2e::E2e;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

fn wait_until(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for fixture");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn failed_restore_wakes_waiter_with_the_holders_reason() {
    let setup = E2e::new("wait-failure");
    let fail = Arc::new(AtomicBool::new(false));
    let changed = Arc::clone(&fail);
    let boat = setup.boat_with_state(200, move |_, s| {
        s["hydrated"] = json!(false);
        if changed.load(Ordering::Acquire) {
            s["state"] = json!("error");
        }
    });
    let start = |log: &str| {
        setup
            .spawn_cmd(&boat)
            .args(["run", "--", "true"])
            .stderr(fs::File::create(setup.root.path().join(log)).unwrap())
            .spawn()
            .unwrap()
    };
    let mut holder = start("holder");
    let read = |name| fs::read_to_string(setup.root.path().join(name)).unwrap_or_default();
    wait_until(|| read("holder").contains("restoring:"));
    let mut waiter = start("waiter");
    wait_until(|| read("waiter").contains("restoring files"));
    fail.store(true, Ordering::Release);
    assert_eq!(holder.wait().unwrap().code(), Some(125));
    assert_eq!(waiter.wait().unwrap().code(), Some(125));
    let stderr = read("waiter");
    assert!(
        stderr.contains("failed: waiting for restore") && stderr.contains("state error"),
        "{stderr}"
    );
    assert!(!setup.event_lines().iter().any(|s| s == "ssh-run"));
}

#[test]
fn only_a_tunnel_user_reboots_and_a_second_reboot_is_refused() {
    let setup = E2e::new("reboot");
    let marker = setup.root.path().join("rebooted");
    let path = marker.clone();
    let boat = setup.boat_with_state(200, move |_, s| {
        if path.exists() {
            s["health"] = json!("ok");
            s["lastReboot"] = json!("new");
        } else {
            s["health"] = json!("degraded");
        }
    });
    // Intercept the scheduled reboot at the SSH boundary.
    let ssh = setup.root.path().join("bin/ssh");
    let script = fs::read_to_string(&ssh).unwrap().replacen("#!/bin/sh\n",
        &format!("#!/bin/sh\nfor a do case \"$a\" in *systemctl\\ reboot) touch '{}'; exit 0;; esac; done\n",marker.display()),1);
    fs::write(ssh, script).unwrap();
    setup.start(&boat);
    setup
        .cmd(&boat)
        .args(["run", "--", "true"])
        .assert()
        .success();
    assert!(!marker.exists());
    let exposed = setup.cmd(&boat).args(["expose", "8080"]).output().unwrap();
    assert!(
        exposed.status.success(),
        "{}",
        String::from_utf8_lossy(&exposed.stderr)
    );
    assert!(marker.exists());
    fs::remove_file(&marker).unwrap();
    let out = setup.cmd(&boat).arg("desktop").output().unwrap();
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("within the last 30 minutes"));
    assert!(!marker.exists());
}
