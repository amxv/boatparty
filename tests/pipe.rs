//! `boatparty pipe` end to end: the input reaches the command byte for
//! byte, nothing boatparty keeps holds it, the end of the input is not the
//! end of the command, and a killed client stops the command although its
//! input is still open.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::e2e::E2e;
use serde_json::Value;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn eventually(limit: Duration, check: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    check()
}

fn pid_in(file: &Path) -> Option<u32> {
    fs::read_to_string(file).ok()?.trim().parse().ok()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else if path.is_file() {
            // Never a FIFO: reading one would block.
            files.push(path);
        }
    }
    files
}

/// Every byte value, a few times over, plus a marker to search for.
fn input(marker: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..=255u8).cycle().take(300 * 1024 + 7).collect();
    bytes.extend_from_slice(marker.as_bytes());
    bytes.extend_from_slice(b"\n\0\r\n");
    bytes
}

#[test]
fn the_input_passes_byte_for_byte_and_is_kept_nowhere() {
    let setup = E2e::new("pipe-exact");
    let boat = setup.boat();
    setup.launch(&boat);
    let marker = "pipe-secret-6f1d9a";
    let bytes = input(marker);
    // The first pipe also installs the runner: the attempt that found it
    // missing must not have eaten any input.
    let output = setup
        .cmd(&boat)
        .args(["pipe", "--", "cat"])
        .write_stdin(bytes.clone())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(output.stdout == bytes, "stdout differs from the input");
    assert!(!text(&output.stderr).contains(marker));

    // The command runs in the checkout with the project's environment.
    let output = setup
        .cmd(&boat)
        .args([
            "pipe",
            "--sh",
            "wc -c | tr -d ' '; pwd; echo \"$BOATPARTY\"; exit 7",
        ])
        .write_stdin(bytes.clone())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{}", text(&output.stderr));
    let lines: Vec<String> = text(&output.stdout).lines().map(String::from).collect();
    assert_eq!(lines[0], bytes.len().to_string());
    assert!(lines[1].ends_with("/remote/app"), "{lines:?}");
    assert_eq!(lines[2], "1");

    // JSON: the frames, then a result with the input size.
    let output = setup
        .cmd(&boat)
        .args(["--json", "pipe", "--", "cat"])
        .write_stdin(format!("{marker}\n"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let frames: Vec<Value> = text(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = frames.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["kind"], "pipe");
    assert_eq!(result["input_bytes"], marker.len() + 1);
    assert!(frames.iter().any(|frame| frame["type"] == "stdout"));

    // Recorded as runs, without the input.
    let runs = fs::read_to_string(setup.home().join("state/runs.jsonl")).unwrap();
    let records: Vec<Value> = runs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let pipes: Vec<&Value> = records.iter().filter(|r| r["kind"] == "pipe").collect();
    assert_eq!(pipes.len(), 3, "{runs}");
    assert_eq!(pipes[0]["command"], "cat");
    assert_eq!(pipes[0]["input_bytes"], bytes.len());
    assert_eq!(pipes[1]["exit_code"], 7);
    for file in walk(&setup.home()) {
        let content = fs::read(&file).unwrap_or_default();
        assert!(
            !text(&content).contains(marker),
            "the input is in {}",
            file.display()
        );
    }
    // Nor is it left on the sandbox's side.
    for file in walk(&setup.remote().join(".boatparty")) {
        let content = fs::read(&file).unwrap_or_default();
        assert!(
            !text(&content).contains(marker),
            "the input is in {}",
            file.display()
        );
    }
}

#[test]
fn the_end_of_the_input_does_not_end_the_command() {
    let setup = E2e::new("pipe-eof");
    let boat = setup.boat();
    setup.launch(&boat);
    let output = setup
        .cmd(&boat)
        .args([
            "pipe",
            "--sh",
            "cat > got.txt; sleep 1; echo done; wc -c < got.txt",
        ])
        .write_stdin("abc")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "done\n3\n");
    // A command that never reads its input still ends normally.
    let output = setup
        .cmd(&boat)
        .args(["pipe", "--", "echo", "unread"])
        .write_stdin(vec![b'x'; 2 * 1024 * 1024])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "unread\n");
}

#[test]
fn a_killed_client_stops_the_command_while_its_input_is_still_open() {
    let setup = E2e::new("pipe-kill");
    let boat = setup.boat();
    setup.launch(&boat);
    let leader_file = setup.root.path().join("leader.pid");
    let child_file = setup.root.path().join("child.pid");
    let line = format!(
        "sleep 300 & echo $! > '{}'; echo $$ > '{}'; cat > /dev/null; wait",
        child_file.display(),
        leader_file.display()
    );
    let mut client = setup
        .spawn_cmd(&boat)
        .args(["pipe", "--sh", &line])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The input stays open (this test holds it) for the whole test.
    let mut input = client.stdin.take().unwrap();
    input.write_all(b"some input\n").unwrap();
    assert!(
        eventually(Duration::from_secs(30), || leader_file.exists()
            && child_file.exists()),
        "the command never started"
    );
    let leader = pid_in(&leader_file).unwrap();
    let child = pid_in(&child_file).unwrap();
    assert!(boatparty::system::pid_alive(leader) && boatparty::system::pid_alive(child));

    let killed_at = Instant::now();
    client.kill().unwrap(); // SIGKILL
    let _ = client.wait();
    assert!(
        eventually(Duration::from_secs(15), || {
            !boatparty::system::pid_alive(leader) && !boatparty::system::pid_alive(child)
        }),
        "the command outlived its SIGKILLed client"
    );
    assert!(
        killed_at.elapsed() < Duration::from_secs(10),
        "{:?}",
        killed_at.elapsed()
    );
    drop(input);
}

#[test]
fn a_cancel_stops_the_command_and_exits_143() {
    let setup = E2e::new("pipe-cancel");
    let boat = setup.boat();
    setup.launch(&boat);
    let leader_file = setup.root.path().join("leader.pid");
    let line = format!("echo $$ > '{}'; sleep 300", leader_file.display());
    let mut client = setup
        .spawn_cmd(&boat)
        .args(["pipe", "--sh", &line])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = client.stdin.take().unwrap();
    assert!(eventually(Duration::from_secs(30), || leader_file.exists()));
    let leader = pid_in(&leader_file).unwrap();
    std::process::Command::new("kill")
        .args(["-TERM", &client.id().to_string()])
        .status()
        .unwrap();
    let status = client.wait().unwrap();
    assert_eq!(status.code(), Some(143));
    assert!(eventually(Duration::from_secs(10), || {
        !boatparty::system::pid_alive(leader)
    }));
    drop(input);
}
