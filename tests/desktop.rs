//! Exercise the image-specific service control and GUI readiness without
//! touching the machine running the tests.
#![cfg(target_os = "linux")]

mod common;

use common::e2e::E2e;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn executable(path: &Path, script: &str) {
    fs::write(path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn wait_for_stop(events: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(events)
            .unwrap_or_default()
            .contains("stop lightdm.service")
        {
            return;
        }
        assert!(Instant::now() < deadline, "no idle desktop shutdown");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_gui_server_inherits_the_session_and_is_ready_on_its_first_window() {
    let setup = E2e::new("desktop-serve");
    let boat = setup.boat();
    let bin = setup.root.path().join("bin");
    let log = setup.root.path().join("services");
    let owner = setup.root.path().join("window-owner");
    executable(&bin.join("ctl"), r#"echo "$*" >> "$SERVICES""#);
    executable(
        &bin.join("pgrep"),
        r#"if [ "$1" = -u ]; then echo "$SESSION_PID"; else exec /usr/bin/pgrep "$@"; fi"#,
    );
    executable(
        &bin.join("xdotool"),
        r#"if [ "$1" = search ]; then echo 17; else cat "$WINDOW_OWNER"; fi"#,
    );
    let mut session = Command::new("sleep")
        .arg("60")
        .env("DISPLAY", ":73")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/test-bus")
        .spawn()
        .unwrap();
    let output = setup.cmd(&boat)
        .env("SESSION_PID", session.id().to_string())
        .env("SERVICES", &log)
        .env("WINDOW_OWNER", &owner)
        .env("BOATPARTY_RUNNER_SYSTEMCTL", bin.join("ctl"))
        .args(["--json","serve","--desktop","--sh",r#"test "$DISPLAY" = :73 && test "$DBUS_SESSION_BUS_ADDRESS" = unix:path=/test-bus || exit 1; echo $$ > "$WINDOW_OWNER"; sleep 1.5"#])
        .output().unwrap();
    session.kill().unwrap();
    session.wait().unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&output.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["desktop"], true);
    assert!(result["ready_seconds"].is_number(), "{result}");
    wait_for_stop(&log);
    let actions = fs::read_to_string(log).unwrap();
    assert!(actions.contains("start lightdm.service"));
}

#[test]
fn an_ordinary_run_stops_streaming_and_the_keep_policy_skips_service_control() {
    let setup = E2e::new("desktop-idle");
    let boat = setup.boat();
    let ctl = setup.root.path().join("bin/ctl");
    let log = setup.root.path().join("services");
    executable(&ctl, r#"echo "$*" >> "$SERVICES""#);
    let run = || {
        setup
            .cmd(&boat)
            .env("SERVICES", &log)
            .env("BOATPARTY_RUNNER_SYSTEMCTL", &ctl)
            .args(["run", "--", "true"])
            .assert()
            .success()
    };
    run();
    wait_for_stop(&log);
    fs::remove_file(&log).unwrap();
    let cfg = setup.repo().join("boatparty.toml");
    let mut text = fs::read_to_string(&cfg).unwrap();
    if text.contains("[sandbox]") {
        text = text.replace("[sandbox]", "[sandbox]\ndesktop = \"keep\"");
    } else {
        text.push_str("\n[sandbox]\ndesktop = \"keep\"\n");
    }
    fs::write(cfg, text).unwrap();
    run();
    assert!(!log.exists());
}
