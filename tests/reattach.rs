//! End-to-end drops around the launch acknowledgement and inside a frame.
#![cfg(unix)]
mod common;
use common::e2e::E2e;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

// Pass through the first stdout and stderr records, then cut an arbitrary
// subsequent record in half. The supervisor must remain alive on the other
// side of the fake transport while this process exits 255.
const PROXY: &str = r#"#!/usr/bin/env python3
import os, signal, subprocess, sys
args = sys.argv[1:]
root = os.path.dirname(os.path.dirname(__file__))
original = os.path.join(root, "bin/ssh-original")
mode = os.getenv("DROP", "")
once = os.path.join(root, "dropped")
if "-G" in args:
    print("controlpath " + os.path.join(root, "socket")); sys.exit(0)
if "-O" in args and args[args.index("-O")+1] == "check" and "TEST_MASTER" in os.environ:
    print("Master running (pid=" + os.environ["TEST_MASTER"] + ")", file=sys.stderr)
    sys.exit(0)
if " reattach app " in args[-1] and mode == "gone":
    sys.exit(98)
if " run app " not in args[-1] or os.path.exists(once):
    os.execv(original, [original] + args)
if mode in ("mux", "before"):
    open(once, "w").close()
    if mode == "mux": print("mux_client_request_session: read from master failed", file=sys.stderr)
    sys.exit(255)
p = subprocess.Popen([original] + args, stdout=subprocess.PIPE)
seen = set()
while True:
    header = p.stdout.read(5)
    if not header: break
    tag, size = header[0], int.from_bytes(header[1:], "big")
    data = p.stdout.read(size)
    if {0, 1} <= seen and size > 16:
        sys.stdout.buffer.write(header + data[:size//2]); sys.stdout.flush()
        open(once, "w").close()
        p.kill(); p.wait(); sys.exit(255)
    sys.stdout.buffer.write(header + data); sys.stdout.flush()
    seen.add(tag)
sys.exit(p.wait())
"#;

fn proxy(setup: &E2e) {
    let ssh = setup.root.path().join("bin/ssh");
    fs::rename(&ssh, ssh.with_file_name("ssh-original")).unwrap();
    fs::write(&ssh, PROXY).unwrap();
    fs::set_permissions(ssh, fs::Permissions::from_mode(0o755)).unwrap();
}

fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_mid_frame_drop_replays_exact_bytes_and_the_original_exit() {
    let setup = E2e::new("reattach-bytes");
    let boat = setup.boat();
    setup.start(&boat);
    proxy(&setup);
    let dir = setup.root.path();
    let counters = dir.join("cpu-stat");
    fs::write(&counters, "cpu 100 0 0 0 0 0 0 0\n").unwrap();
    let command = format!(
        "echo once >> '{0}/count'; printf prefix; sleep 0.3; \
         perl -e 'print STDERR chr(127) x 180000; print map chr($_ % 256), 0..270000'; \
         mv '{1}/.boatparty/out' '{1}/.boatparty/retired-out'; mkdir '{1}/.boatparty/out'; \
         echo cpu 175 0 0 0 0 0 0 25 > \"$BOATPARTY_RUNNER_PROC_STAT\"; echo changed > from-run; exit 37",
        dir.display(),
        setup.remote().display()
    );
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("DROP", "middle")
        .env("BOATPARTY_RUNNER_PROC_STAT", &counters)
        .args(["run", "--sh", &command])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        stderr.replace(char::from(127), "")
    );
    assert!(dir.join("dropped").exists());
    assert_eq!(fs::read_to_string(dir.join("count")).unwrap(), "once\n");
    let mut expected = b"prefix".to_vec();
    expected.extend((0..=270000).map(|i| (i % 256) as u8));
    assert!(
        output.stdout == expected,
        "stdout differs at {:?}",
        output
            .stdout
            .iter()
            .zip(&expected)
            .position(|(a, b)| a != b)
    );
    assert_eq!(output.stderr.iter().filter(|b| **b == 127).count(), 180000);
    assert_eq!(
        fs::read_to_string(setup.repo().join("from-run")).unwrap(),
        "changed\n"
    );
    let record = fs::read_to_string(setup.home().join("state/runs.jsonl")).unwrap();
    let record: serde_json::Value = serde_json::from_str(record.trim()).unwrap();
    assert_eq!(record["reattached"], true);
    assert_eq!(record["cpu_steal_percent"], 25);
    assert!(stderr.contains("25% CPU steal"));
}

#[test]
fn missing_record_after_permission_reports_lost_without_rerunning() {
    let setup = E2e::new("reattach-gone");
    let boat = setup.boat();
    setup.start(&boat);
    proxy(&setup);
    let count = setup.root.path().join("count");
    let line = format!(
        "echo once >> '{}'; echo start; sleep 0.3; perl -e 'print \"x\" x 70000'",
        count.display()
    );
    let output = setup
        .cmd(&boat)
        .env("DROP", "gone")
        .env("FAKE_SSH_CHECK", "0")
        .args(["run", "--sh", &line])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&output.stderr).contains("command lost"));
    assert_eq!(fs::read_to_string(count).unwrap(), "once\n");
}

#[test]
fn a_connection_refused_before_permission_can_repeat_the_sync() {
    let setup = E2e::new("reattach-before");
    let boat = setup.boat();
    setup.start(&boat);
    proxy(&setup);
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("DROP", "before")
        .args(["--json", "run", "--", "echo", "once"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let last = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .last()
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(result["retried"], true);
    assert_eq!(result["reattached"], false);
}

#[test]
fn a_stubborn_mux_is_terminated_and_its_socket_unlinked() {
    let setup = E2e::new("reattach-mux");
    let boat = setup.boat();
    setup.start(&boat);
    proxy(&setup);
    let socket = setup.root.path().join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut master = Command::new("sleep").arg("30").spawn().unwrap();
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("DROP", "mux")
        .env("TEST_MASTER", master.id().to_string())
        .args(["run", "--", "echo", "once"])
        .output()
        .unwrap();
    assert!(master.try_wait().unwrap().is_some());
    assert!(!socket.exists());
    assert_eq!(output.stdout, b"once\n");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("replaced a stale SSH control master")
    );
}

#[test]
fn abandoned_run_kills_even_a_child_that_ignores_term() {
    let setup = E2e::new("reattach-orphan");
    let boat = setup.boat();
    setup.start(&boat);
    let pulse = setup.root.path().join("pulse");
    let line = format!(
        "(trap '' TERM; while :; do echo . >> '{}'; sleep 0.1; done) & wait",
        pulse.display()
    );
    let jobs = setup.root.path().join("jobs");
    let mut client = setup
        .spawn_cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_RUNNER_JOBS", &jobs)
        .env("BOATPARTY_RUNNER_REATTACH_SECS", "2")
        .args(["run", "--sh", &line])
        .spawn()
        .unwrap();
    assert!(eventually(|| pulse.exists()));
    client.kill().unwrap();
    client.wait().unwrap();
    assert!(eventually(|| fs::read_dir(&jobs)
        .unwrap()
        .flatten()
        .any(|j| j.path().join("ended").exists())));
    let length = fs::metadata(&pulse).unwrap().len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(fs::metadata(pulse).unwrap().len(), length);
}

#[test]
fn sigterm_cancels_a_detached_run_before_its_lease_expires() {
    let setup = E2e::new("reattach-cancel");
    let boat = setup.boat();
    setup.start(&boat);
    let started = setup.root.path().join("started");
    let line = format!(
        "echo changed >> a.txt; echo ready > '{}'; exec sleep 120",
        started.display()
    );
    let mut client = setup
        .spawn_cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["run", "--sh", &line])
        .spawn()
        .unwrap();
    assert!(eventually(|| started.exists()));
    let pid = rustix::process::Pid::from_raw(client.id().try_into().unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    assert!(eventually(|| client.try_wait().unwrap().is_some()));
    assert_eq!(client.wait().unwrap().code(), Some(143));
    assert_eq!(
        fs::read_to_string(setup.repo().join("a.txt")).unwrap(),
        "one\n"
    );
    let jobs = setup.root.path().join("jobs");
    assert!(
        fs::read_dir(jobs)
            .unwrap()
            .flatten()
            .any(|job| job.path().join("cancel").exists() && job.path().join("ended").exists())
    );
}
