//! Running commands: the runner's `run` mode against scratch repositories,
//! copy-back, cancellation and client death, and `run` end to end with a
//! fake `ssh` that runs the runner locally and a fake boat API that counts
//! provider calls.

#![cfg(unix)] // These fixtures execute the Linux runner or POSIX tool shims locally.

mod common;

use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use boatparty::runner::{RunnerHost, Trailer, TrailerFilter};
use boatparty::sync::{
    self, CancelProbe, CancelState, History, MirrorRequest, PullBack, RunEnd, RunOutcome, Sink,
    WorktreeInfo,
};
use common::runner::{LocalRunner, base_repo, git, write};
use common::{TempDir, boatparty};
use serde_json::{Value, json};

/// Collects what a run printed.
#[derive(Default)]
struct Collect {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    events: Vec<String>,
}

impl Sink for Collect {
    fn stdout(&mut self, bytes: &[u8]) {
        self.stdout.extend_from_slice(bytes);
    }

    fn stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
    }

    fn event(&mut self, name: &str) {
        self.events.push(name.to_string());
    }
}

/// Cancels softly once the command has started.
struct CancelAfterStart<'a> {
    started: &'a Cell<bool>,
}

impl CancelProbe for CancelAfterStart<'_> {
    fn state(&self) -> CancelState {
        if self.started.get() {
            CancelState::Soft
        } else {
            CancelState::None
        }
    }
}

struct Run {
    outcome: RunOutcome,
    info: WorktreeInfo,
    printed: Collect,
}

/// The client's side of one `run`, with the runner run locally.
fn run(local: &Path, host: &LocalRunner, known: &mut Vec<String>, command: &str) -> Run {
    let info = WorktreeInfo::read(local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let refs = sync::local_refs(&info).unwrap();
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: true,
        known,
        history: History::Full,
        seed: None,
    };
    let mut printed = Collect::default();
    let outcome =
        sync::run_command(host, &request, command, &mut printed, &sync::NeverCancel).unwrap();
    if !outcome.known.is_empty() {
        *known = outcome.known.clone();
    }
    Run {
        outcome,
        info,
        printed,
    }
}

fn done(run: &Run) -> (i32, u64) {
    match run.outcome.end {
        RunEnd::Done {
            exit_code,
            patch_bytes,
            ..
        } => (exit_code, patch_bytes),
        ref other => panic!("no result: {other:?}; stderr {}", text(&run.printed.stderr)),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn output_streams_the_exit_code_propagates_and_the_command_runs_in_the_checkout() {
    let root = TempDir::new("run-exit");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut known = Vec::new();
    let run = run(
        &local,
        &host,
        &mut known,
        "cat a.txt; echo err >&2; printf '\\000\\377' ; exit 7",
    );
    assert_eq!(done(&run), (7, 0));
    assert_eq!(run.printed.stdout, b"one\n\x00\xff");
    assert_eq!(text(&run.printed.stderr), "err\n");
    assert_eq!(run.printed.events, ["started"]);
    assert!(!run.outcome.cancelled);
    assert!(known.contains(&run.info.head));
}

#[test]
fn a_command_that_edits_files_sends_a_patch_that_applies_locally() {
    let root = TempDir::new("run-patch");
    let local = base_repo(root.path());
    write(&local.join("a.txt"), "one\nedited\n");
    let host = LocalRunner::new(root.path(), "remote");
    let mut known = Vec::new();
    let run = run(&local, &host, &mut known, "printf 'formatted\\n' >> a.txt");
    let (code, bytes) = done(&run);
    assert_eq!(code, 0);
    assert!(bytes > 0);
    let patch = sync::fetch_patch(&host, "app", &run.outcome.nonce).unwrap();
    assert_eq!(patch.len() as u64, bytes);
    let applied = sync::apply_pullback(&run.info, &patch).unwrap();
    assert_eq!(
        applied,
        PullBack::Applied {
            files: vec!["a.txt".into()]
        }
    );
    assert_eq!(
        fs::read_to_string(local.join("a.txt")).unwrap(),
        "one\nedited\nformatted\n"
    );
    // The patch is gone from the sandbox once fetched.
    assert!(
        !host
            .state
            .join(format!("out/{}.patch", run.outcome.nonce))
            .exists()
    );
}

#[test]
fn a_local_edit_to_the_same_lines_turns_the_copy_back_into_a_kept_conflict_patch() {
    let root = TempDir::new("run-conflict");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let mut known = Vec::new();
    let run = run(
        &local,
        &host,
        &mut known,
        "printf 'remote\\n' >> a.txt; exit 3",
    );
    let (code, _) = done(&run);
    assert_eq!(code, 3);
    write(&local.join("a.txt"), "one\nlocal\n");
    let patch = sync::fetch_patch(&host, "app", &run.outcome.nonce).unwrap();
    let PullBack::Conflict { files, kept, .. } = sync::apply_pullback(&run.info, &patch).unwrap()
    else {
        panic!("the patch applied over a conflicting local edit");
    };
    assert_eq!(files, ["a.txt"]);
    assert!(kept.starts_with(&run.info.git_dir));
    assert!(
        kept.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("boatparty-pullback-")
    );
    assert_eq!(fs::read(&kept).unwrap(), patch);
    assert_eq!(
        fs::read_to_string(local.join("a.txt")).unwrap(),
        "one\nlocal\n"
    );
}

#[test]
fn a_command_re_synced_mid_run_reports_only_its_own_changes() {
    let root = TempDir::new("run-resync");
    let local = base_repo(root.path());
    let remote = LocalRunner::new(root.path(), "remote");
    let mut known = Vec::new();
    run(&local, &remote, &mut known, "true");

    std::thread::scope(|scope| {
        let long_known = known.clone();
        let local_ref = &local;
        let root_path = root.path();
        let long = scope.spawn(move || {
            // The same sandbox directories, through a host of this thread's own.
            let host = LocalRunner::new(root_path, "remote");
            let mut known = long_known;
            let run = run(
                local_ref,
                &host,
                &mut known,
                "sleep 2; printf 'formatted later\\n' >> a.txt",
            );
            let (code, _) = done(&run);
            (code, run.outcome.nonce, text(&run.printed.stderr))
        });
        std::thread::sleep(Duration::from_millis(900));
        write(&local.join("second.txt"), "second sync\n");
        let mut known_now = known.clone();
        let second = run(&local, &remote, &mut known_now, "true");
        assert_eq!(done(&second).0, 0);
        assert!(remote.repo.join("second.txt").exists());

        let (code, nonce, stderr) = long.join().unwrap();
        assert_eq!(code, 0);
        assert!(
            stderr.contains("re-synced while this command ran"),
            "{stderr}"
        );
        let patch = text(&sync::fetch_patch(&remote, "app", &nonce).unwrap());
        assert!(patch.contains("formatted later"), "{patch}");
        assert!(!patch.contains("second.txt"), "{patch}");
    });
}

#[test]
fn a_cancel_stops_the_command_and_brings_nothing_back() {
    let root = TempDir::new("run-cancel");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let info = WorktreeInfo::read(&local).unwrap();
    let snapshot = sync::snapshot(&info).unwrap();
    let refs = sync::local_refs(&info).unwrap();
    let request = MirrorRequest {
        project: "app",
        info: &info,
        snapshot: &snapshot,
        refs: &refs,
        send_refs: true,
        known: &[],
        history: History::Full,
        seed: None,
    };
    let started = Cell::new(false);
    struct Watch<'a> {
        inner: Collect,
        started: &'a Cell<bool>,
    }
    impl Sink for Watch<'_> {
        fn stdout(&mut self, bytes: &[u8]) {
            self.inner.stdout(bytes);
        }
        fn stderr(&mut self, bytes: &[u8]) {
            self.inner.stderr(bytes);
        }
        fn event(&mut self, name: &str) {
            self.started.set(true);
            self.inner.event(name);
        }
    }
    let mut sink = Watch {
        inner: Collect::default(),
        started: &started,
    };
    let began = Instant::now();
    let outcome = sync::run_command(
        &host,
        &request,
        "echo changed >> a.txt; sleep 60",
        &mut sink,
        &CancelAfterStart { started: &started },
    )
    .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "{:?}",
        began.elapsed()
    );
    assert!(outcome.cancelled);
    match outcome.end {
        RunEnd::Done {
            exit_code,
            patch_bytes,
            ..
        } => {
            assert_eq!(exit_code, 143, "the command group got SIGTERM");
            assert_eq!(patch_bytes, 0, "a cancelled command sends no changes back");
        }
        other => panic!(
            "no result after a cancel: {other:?} in {:?}; stderr {}",
            began.elapsed(),
            text(&sink.inner.stderr)
        ),
    }
}

/// Wait until `check` holds or `limit` passes.
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

#[test]
fn end_of_stdin_terminates_the_commands_whole_process_group() {
    let root = TempDir::new("run-death");
    let local = base_repo(root.path());
    let host = LocalRunner::new(root.path(), "remote");
    let head = git(&local, &["rev-parse", "HEAD"]);
    // Sync once so the run needs no pack.
    run(&local, &host, &mut Vec::new(), "true");
    let child_file = root.path().join("child.pid");
    let command = format!(
        "sleep 120 & echo $! > '{}'; sleep 120",
        child_file.display()
    );
    let mut runner = host
        .command(&[
            "attached-run".into(),
            "app".into(),
            "ndeath".into(),
            "0".into(),
            head.clone(),
            head,
            "main".into(),
            "keep".into(),
            "full".into(),
            "-".into(),
            command,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = runner.stdin.take().unwrap();
    // An empty credentials block, then an empty refs block.
    stdin.write_all(b"\n\n").unwrap();
    let pid_file = host.state.join("out/ndeath.pid");
    assert!(eventually(Duration::from_secs(10), || {
        child_file.exists() && pid_file.exists()
    }));
    let leader: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let child: u32 = fs::read_to_string(&child_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(boatparty::system::pid_alive(leader));
    assert!(boatparty::system::pid_alive(child));

    // The client dies: its end of stdin closes.
    let killed_at = Instant::now();
    drop(stdin);
    assert!(
        eventually(Duration::from_secs(10), || {
            !boatparty::system::pid_alive(leader) && !boatparty::system::pid_alive(child)
        }),
        "the command's process group outlived its client"
    );
    assert!(
        killed_at.elapsed() < Duration::from_secs(5),
        "{:?}",
        killed_at.elapsed()
    );
    let output = runner.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(143));
    let mut filter = TrailerFilter::new("ndeath");
    filter.push(&output.stderr);
    assert!(matches!(
        filter.result(),
        Some(Trailer::Done {
            exit_code: 143,
            patch_bytes: 0,
            ..
        })
    ));
}

// ----- `run` end to end -----------------------------------------------

use common::e2e::{E2e, ID};

#[test]
fn a_warm_cheers_makes_no_provider_call_before_the_command_and_brings_changes_back() {
    let setup = E2e::new("run-warm");
    let boat = setup.boat();
    setup.start(&boat);
    let _ = fs::remove_file(setup.events());

    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args([
            "run",
            "--",
            "sh",
            "-c",
            "echo out; echo err >&2; printf 'appended\\n' >> a.txt; exit 7",
        ])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(7), "{stderr}");
    assert_eq!(text(&output.stdout), "out\n");
    assert!(stderr.contains("err\n"), "{stderr}");
    assert!(
        stderr.contains("applied the command's changes locally: a.txt"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(setup.repo().join("a.txt")).unwrap(),
        "one\nappended\n"
    );

    // The heartbeat and the idle helper run in the background, after the
    // command started; nothing talked to the provider before it.
    assert!(
        eventually(Duration::from_secs(10), || {
            setup
                .event_lines()
                .iter()
                .any(|line| line.contains("PATCH") && line.contains("\"ttlSeconds\":300"))
        }),
        "no idle deadline: {:#?}",
        setup.event_lines()
    );
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

    // The run is recorded and the lease is gone.
    let log = setup.cmd(&boat).args(["log", "--json"]).output().unwrap();
    let document: Value = serde_json::from_slice(&log.stdout).unwrap();
    let run = &document["runs"][0];
    assert_eq!(run["exit_code"], 7);
    assert_eq!(run["warm"], true);
    assert_eq!(run["copy_back"], "applied");
    assert!(run["prep_seconds"].as_f64().is_some());
    let state: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("state/state.json")).unwrap())
            .unwrap();
    let member = &state["projects"]["app"]["members"][0];
    assert_eq!(member["leases"], json!([]));
    assert_eq!(member["last_run"]["exit_code"], 7);
    assert_eq!(member["sync"]["mode"], "full");
}

#[test]
fn cheers_json_streams_frames_and_ends_with_one_result() {
    let setup = E2e::new("run-json");
    let boat = setup.boat();
    setup.start(&boat);
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "run", "--sh", "echo out; echo err >&2; exit 3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let frames: Vec<Value> = text(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is a frame"))
        .collect();
    let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.last(), Some(&"result"));
    assert_eq!(kinds.iter().filter(|kind| **kind == "result").count(), 1);
    assert!(frames.contains(&json!({"type": "event", "event": "started"})));
    let started = frames
        .iter()
        .position(|frame| frame["type"] == "event")
        .unwrap();
    let first_output = frames
        .iter()
        .position(|frame| frame["type"] == "stderr" || frame["type"] == "stdout")
        .unwrap();
    assert!(
        started < first_output,
        "output came before the start event: {frames:#?}"
    );
    let joined = |kind: &str| -> String {
        frames
            .iter()
            .filter(|frame| frame["type"] == kind)
            .map(|frame| frame["data"].as_str().unwrap())
            .collect()
    };
    assert_eq!(joined("stdout"), "out\n");
    assert_eq!(joined("stderr"), "err\n");
    let result = frames.last().unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["exit_code"], 3);
    assert_eq!(result["member"]["id"], ID);
    assert_eq!(result["copy_back"]["status"], "none");
}

#[test]
fn cheers_without_a_command_is_a_usage_error() {
    let root = TempDir::new("run-usage");
    let output = boatparty(root.path()).arg("run").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
