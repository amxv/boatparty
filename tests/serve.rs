//! Servers, forwards and public URLs: the runner's `serve` mode against
//! scratch repositories (its serve marker, flooring beside it, the ready
//! event), and `cruise` and `expose` end to end with a fake `ssh` that runs
//! the runner locally and a fake boat API, including a client killed with
//! SIGKILL.

mod common;

use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use boatparty::project::ProjectConfig;
use boatparty::runner::{RunnerHost, RunnerSpec};
use boatparty::sync::{self, History, MirrorRequest, RunEnd, Sink, WorktreeInfo};
use common::TempDir;
use common::e2e::{E2e, HOST_TOKEN, ID};
use common::runner::{LocalRunner, base_repo, git, write};
use serde_json::Value;

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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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

fn pid_in(file: &Path) -> Option<u32> {
    fs::read_to_string(file).ok()?.trim().parse().ok()
}

/// A port nothing listens on now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A shell line that listens on `port` for `seconds`, then exits 0.
fn listener(port: u16, seconds: u32) -> String {
    format!(
        "perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new(LocalAddr => q(127.0.0.1), \
         LocalPort => {port}, Listen => 1, ReuseAddr => 1) or die qq(listen: $!); sleep {seconds}'"
    )
}

fn nanos(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}

fn set_fractional_mtime(path: &Path) {
    let time = SystemTime::now() - Duration::from_millis(1500) + Duration::from_nanos(123_456_789);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// A scratch member whose spec floors `out/`.
struct Member {
    _root: TempDir,
    local: std::path::PathBuf,
    host: LocalRunner,
    known: Vec<String>,
}

impl Member {
    fn new(label: &str) -> Self {
        let root = TempDir::new(label);
        let local = base_repo(root.path());
        write(&local.join(".gitignore"), "ignored/\nout/\n");
        git(&local, &["add", "-A"]);
        git(&local, &["commit", "-qm", "ignore out"]);
        let host = LocalRunner::new(root.path(), "remote");
        let config = "version = 1\n[project]\nname = \"app\"\n[build]\noutput_dirs = [\"out\"]\n";
        let project = ProjectConfig::parse(config, Path::new("boatparty.toml")).unwrap();
        *host.spec.borrow_mut() = Some(RunnerSpec::new(project.config()));
        Self {
            _root: root,
            local,
            host,
            known: Vec::new(),
        }
    }

    /// Run `command` like `cheers`, or as a server with `ready_port`.
    fn run(&mut self, command: &str, serve: Option<Option<u16>>) -> (i32, Collect) {
        let info = WorktreeInfo::read(&self.local).unwrap();
        let snapshot = sync::snapshot(&info).unwrap();
        let refs = sync::local_refs(&info).unwrap();
        let request = MirrorRequest {
            project: "app",
            info: &info,
            snapshot: &snapshot,
            refs: &refs,
            send_refs: true,
            known: &self.known,
            history: History::Full,
            seed: None,
        };
        let mut printed = Collect::default();
        let credentials = sync::RunCredentials::none();
        let outcome = match serve {
            Some(ready_port) => sync::serve_command_with(
                &self.host,
                &request,
                command,
                &credentials,
                ready_port,
                &mut printed,
                &sync::NeverCancel,
            ),
            None => sync::run_command(
                &self.host,
                &request,
                command,
                &mut printed,
                &sync::NeverCancel,
            ),
        }
        .unwrap();
        if !outcome.known.is_empty() {
            self.known = outcome.known.clone();
        }
        match outcome.end {
            RunEnd::Done { exit_code, .. } => (exit_code, printed),
            other => panic!("no result: {other:?}; stderr {}", text(&printed.stderr)),
        }
    }

    /// Start `command` in the runner's `serve` mode by hand; returns the
    /// runner process (its stdin is the liveness channel) and the nonce.
    fn spawn_server(&self, nonce: &str, command: &str) -> std::process::Child {
        let head = git(&self.local, &["rev-parse", "HEAD"]);
        let mut runner = self
            .host
            .command(&[
                "serve".into(),
                "app".into(),
                nonce.into(),
                "0".into(),
                head.clone(),
                head,
                "main".into(),
                "keep".into(),
                "full".into(),
                "-".into(),
                command.into(),
                "-".into(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // An empty credentials block, then an empty refs block.
        runner.stdin.as_mut().unwrap().write_all(b"\n\n").unwrap();
        runner
    }
}

#[test]
fn a_running_server_is_marked_and_commands_beside_it_skip_flooring() {
    let mut member = Member::new("serve-floor");
    // A plain sync first, so the server needs no pack (its tree is HEAD's).
    assert_eq!(member.run("true", None).0, 0);
    let pid_file = member.local.parent().unwrap().join("server.pid");
    let mut server = member.spawn_server(
        "nserve",
        &format!("echo $$ > '{}'; sleep 120", pid_file.display()),
    );
    let marker = member.host.state.join("out/nserve.serve");
    assert!(eventually(Duration::from_secs(10), || marker.exists()
        && pid_file.exists()));
    assert_eq!(
        pid_in(&marker),
        pid_in(&pid_file),
        "the marker holds its pid"
    );

    // A command beside it builds: its outputs keep their times.
    let output = member.host.repo.join("out/app");
    let (code, printed) = member.run("mkdir -p out && printf o > out/app", None);
    assert_eq!(code, 0, "{}", text(&printed.stderr));
    set_fractional_mtime(&output);
    let (code, _) = member.run("true", None);
    assert_eq!(code, 0);
    assert_ne!(nanos(&output), 0, "floored while a server ran");

    // The server stops when its client goes; its marker goes with it.
    drop(server.stdin.take());
    let status = server.wait().unwrap();
    assert_eq!(status.code(), Some(143));
    assert!(!marker.exists(), "the serve marker outlived the server");

    // Without a server, flooring is back.
    set_fractional_mtime(&output);
    let (code, _) = member.run("touch out/app", None);
    assert_eq!(code, 0);
    assert_eq!(nanos(&output), 0, "not floored after the server ended");
}

#[test]
fn end_of_stdin_stops_a_servers_whole_process_group_even_if_it_ignores_term() {
    let mut member = Member::new("serve-death");
    assert_eq!(member.run("true", None).0, 0);
    let root = member.local.parent().unwrap().to_path_buf();
    let leader_file = root.join("leader.pid");
    let child_file = root.join("child.pid");
    // A child that ignores TERM must still go (KILL follows).
    let mut server = member.spawn_server(
        "ndeath",
        &format!(
            "sh -c 'trap \"\" TERM; echo $$ > {child}; while :; do sleep 1; done' & \
             echo $$ > '{leader}'; wait",
            child = child_file.display(),
            leader = leader_file.display()
        ),
    );
    assert!(eventually(Duration::from_secs(10), || {
        leader_file.exists() && child_file.exists()
    }));
    let leader = pid_in(&leader_file).unwrap();
    let child = pid_in(&child_file).unwrap();
    assert!(boatparty::system::pid_alive(leader) && boatparty::system::pid_alive(child));

    // The runner's parent (the client's connection) dies: stdin ends.
    let killed_at = Instant::now();
    drop(server.stdin.take());
    assert!(
        eventually(Duration::from_secs(15), || {
            !boatparty::system::pid_alive(leader) && !boatparty::system::pid_alive(child)
        }),
        "the server's process group outlived its client"
    );
    assert!(
        killed_at.elapsed() < Duration::from_secs(10),
        "{:?}",
        killed_at.elapsed()
    );
    let _ = server.wait();
}

#[test]
fn a_server_reports_ready_once_its_port_listens() {
    let mut member = Member::new("serve-ready");
    let port = free_port();
    let (code, printed) = member.run(&listener(port, 3), Some(Some(port)));
    assert_eq!(code, 0, "{}", text(&printed.stderr));
    let ready: Vec<&String> = printed
        .events
        .iter()
        .filter(|name| *name == "ready")
        .collect();
    assert_eq!(ready.len(), 1, "{:?}", printed.events);
    let started = printed.events.iter().position(|name| name == "started");
    let ready_at = printed.events.iter().position(|name| name == "ready");
    assert!(started < ready_at, "{:?}", printed.events);

    // Without a ready port there is no ready event; a port that never
    // listens gives none either.
    let (_, printed) = member.run("true", Some(None));
    assert!(!printed.events.contains(&"ready".to_string()));
    let (_, printed) = member.run("sleep 1", Some(Some(free_port())));
    assert!(!printed.events.contains(&"ready".to_string()));
}

// ----- `cruise` and `expose` end to end ---------------------------------

fn state_text(setup: &E2e) -> String {
    fs::read_to_string(setup.home().join("state/state.json")).unwrap_or_default()
}

fn runs_text(setup: &E2e) -> String {
    fs::read_to_string(setup.home().join("state/runs.jsonl")).unwrap_or_default()
}

#[test]
fn cruise_stops_the_server_within_seconds_after_its_client_is_killed() {
    let setup = E2e::new("cruise-kill");
    let boat = setup.boat();
    setup.launch(&boat);
    let leader_file = setup.root.path().join("leader.pid");
    let child_file = setup.root.path().join("child.pid");
    let line = format!(
        "sleep 300 & echo $! > '{}'; echo $$ > '{}'; wait",
        child_file.display(),
        leader_file.display()
    );
    let log = fs::File::create(setup.root.path().join("client.err")).unwrap();
    let mut client = setup
        .spawn_cmd(&boat)
        .args(["cruise", "--sh", &line])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let client_log = || text(&fs::read(setup.root.path().join("client.err")).unwrap_or_default());
    assert!(
        eventually(Duration::from_secs(30), || leader_file.exists()
            && child_file.exists()),
        "the server never started: {}",
        client_log()
    );
    let leader = pid_in(&leader_file).unwrap();
    let child = pid_in(&child_file).unwrap();
    assert!(boatparty::system::pid_alive(leader) && boatparty::system::pid_alive(child));
    // It holds a serve lease while it runs.
    let state: Value = serde_json::from_str(&state_text(&setup)).unwrap();
    assert_eq!(
        state["projects"]["app"]["members"][0]["leases"][0]["kind"], "serve",
        "{state}"
    );

    let killed_at = Instant::now();
    client.kill().unwrap(); // SIGKILL
    let _ = client.wait();
    assert!(
        eventually(Duration::from_secs(15), || {
            !boatparty::system::pid_alive(leader) && !boatparty::system::pid_alive(child)
        }),
        "the server outlived its SIGKILLed client: {}",
        client_log()
    );
    assert!(
        killed_at.elapsed() < Duration::from_secs(10),
        "{:?}",
        killed_at.elapsed()
    );
}

#[test]
fn cruise_reports_ready_forwards_on_its_own_connection_and_records_a_serve_run() {
    let setup = E2e::new("cruise-ready");
    let boat = setup.boat();
    setup.launch(&boat);
    let port = free_port();
    let local = free_port();
    let output = setup
        .cmd(&boat)
        .args([
            "--json",
            "cruise",
            "--forward",
            &format!("{local}:{port}"),
            "--ready-port",
            &port.to_string(),
            "--sh",
            &listener(port, 3),
        ])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let frames: Vec<Value> = text(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is a frame"))
        .collect();
    let events: Vec<&str> = frames
        .iter()
        .filter(|frame| frame["type"] == "event")
        .filter_map(|frame| frame["event"].as_str())
        .collect();
    assert_eq!(
        events.iter().filter(|name| **name == "ready").count(),
        1,
        "{frames:#?}"
    );
    assert_eq!(
        stderr.matches("is listening on the sandbox").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("here at http://127.0.0.1:{local}")),
        "{stderr}"
    );
    let result = frames.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["kind"], "serve");
    assert_eq!(result["capped"], false);
    assert!(result["ready_seconds"].as_f64().is_some(), "{result}");
    assert_eq!(result["forwards"][0]["bind"], "127.0.0.1");
    assert_eq!(result["forwards"][0]["listen"], local);
    assert_eq!(result["forwards"][0]["target"], port);
    assert_eq!(result["max_minutes"], 120.0);

    // The server's invocation went over a connection of its own, with the
    // forward bound to loopback; nothing else asked for one.
    let serve_lines: Vec<String> = setup
        .event_lines()
        .into_iter()
        .filter(|line| line.starts_with("ssh-serve"))
        .collect();
    // (A first attempt that found the runner missing adds a line.)
    assert!(
        !serve_lines.is_empty() && serve_lines.len() <= 2,
        "{serve_lines:#?}"
    );
    for line in &serve_lines {
        assert!(line.contains("ControlPath=none"), "{line}");
        assert!(line.contains("ControlMaster=no"), "{line}");
        assert!(line.contains("ExitOnForwardFailure=yes"), "{line}");
        assert!(
            line.contains(&format!("-L 127.0.0.1:{local}:127.0.0.1:{port}")),
            "{line}"
        );
        assert!(!line.contains("ProxyCommand=false"), "{line}");
    }

    let runs = runs_text(&setup);
    let record: Value = serde_json::from_str(runs.lines().last().unwrap()).unwrap();
    assert_eq!(record["kind"], "serve");
    assert_eq!(record["exit_code"], 0);
    let state: Value = serde_json::from_str(&state_text(&setup)).unwrap();
    let member = &state["projects"]["app"]["members"][0];
    assert_eq!(member["leases"], serde_json::json!([]));
    assert_eq!(member["last_run"]["kind"], "serve");
}

#[test]
fn the_time_cap_stops_a_forgotten_server() {
    let setup = E2e::new("cruise-cap");
    let boat = setup.boat();
    setup.launch(&boat);
    let began = Instant::now();
    let output = setup
        .cmd(&boat)
        .args([
            "--json",
            "cruise",
            "--max-minutes",
            "0.02",
            "--",
            "sleep",
            "120",
        ])
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert!(began.elapsed() < Duration::from_secs(30), "{stderr}");
    assert_eq!(output.status.code(), Some(143), "{stderr}");
    assert!(
        stderr.contains("stopping the server after 0.02 minutes"),
        "{stderr}"
    );
    let result: Value = serde_json::from_str(text(&output.stdout).lines().last().unwrap()).unwrap();
    assert_eq!(result["capped"], true);
    assert_eq!(result["cancelled"], false);
    let record: Value = serde_json::from_str(runs_text(&setup).lines().last().unwrap()).unwrap();
    assert_eq!(record["capped"], true);
}

#[test]
fn a_taken_local_port_fails_before_anything_runs() {
    let setup = E2e::new("cruise-port");
    let boat = setup.boat();
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let output = setup
        .cmd(&boat)
        .args([
            "--json",
            "cruise",
            "--forward",
            &format!("{port}:80"),
            "--",
            "true",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let result: Value = serde_json::from_str(text(&output.stdout).lines().last().unwrap()).unwrap();
    assert_eq!(result["error"]["code"], "port_in_use");
    assert!(
        !setup
            .event_lines()
            .iter()
            .any(|line| line.starts_with("ssh-serve"))
    );
}

#[test]
fn expose_prints_the_gated_url_on_stdout_only() {
    let setup = E2e::new("expose");
    let boat = setup.boat();
    setup.launch(&boat);
    let output = setup
        .cmd(&boat)
        .args(["expose", "8000", "--title", "app"])
        .output()
        .unwrap();
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout,
        format!("https://app-8000.on.boat.example?_token={HOST_TOKEN}\n")
    );
    assert!(stderr.contains("access token"), "{stderr}");
    assert!(!stderr.contains(HOST_TOKEN), "the token reached a log line");
    assert!(setup.event_lines().iter().any(|line| {
        line.starts_with(&format!("provider POST /sandboxes/{ID}/host"))
            && line.contains("\"public\":false")
            && line.contains("\"title\":\"app\"")
    }));

    let json = setup
        .cmd(&boat)
        .args(["--json", "expose", "8000", "--localhost"])
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(0));
    let document: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(document["protected"], true);
    assert_eq!(document["access"], "private");
    assert_eq!(document["localhost"], true);
    assert!(document["url"].as_str().unwrap().contains(HOST_TOKEN));
    assert!(!text(&json.stderr).contains(HOST_TOKEN));

    let public = setup
        .cmd(&boat)
        .args(["expose", "8000", "--public"])
        .output()
        .unwrap();
    assert_eq!(text(&public.stdout), "https://app-8000.on.boat.example\n");
    assert!(text(&public.stderr).contains("public"));

    // Never written anywhere boatparty keeps things.
    assert!(!state_text(&setup).contains(HOST_TOKEN));
    assert!(!runs_text(&setup).contains(HOST_TOKEN));
    for entry in walk(&setup.home()) {
        let bytes = fs::read(&entry).unwrap_or_default();
        assert!(
            !text(&bytes).contains(HOST_TOKEN),
            "the token is in {}",
            entry.display()
        );
    }
}

#[test]
fn reserved_ports_are_refused_before_any_call() {
    let setup = E2e::new("expose-reserved");
    let boat = setup.boat();
    for port in ["22", "6080", "8090", "8911", "47990"] {
        let output = setup.cmd(&boat).args(["expose", port]).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{port}");
        assert!(text(&output.stderr).contains("reserved"));
    }
    assert!(
        boat.requests().is_empty(),
        "a reserved port reached the provider"
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn a_forward_whose_connection_drops_reconnects_until_cancelled() {
    let setup = E2e::new("forward-reconnect");
    let boat = setup.boat();
    setup.launch(&boat);
    // In front of the harness's ssh: the first connection carrying a
    // forward ends a moment after it is up; later ones hold.
    let bin = setup.root.path().join("bin");
    let real = bin.join("ssh-real");
    fs::rename(bin.join("ssh"), &real).unwrap();
    let count = setup.root.path().join("forward-connections");
    let wrapper = format!(
        "#!/bin/sh\n\
         case \" $* \" in *' -L '*)\n\
           n=$(cat '{count}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{count}'\n\
           if [ $n -eq 1 ]; then echo ready; sleep 0.3; exit 255; fi;;\n\
         esac\n\
         exec '{real}' \"$@\"\n",
        count = count.display(),
        real = real.display(),
    );
    fs::write(bin.join("ssh"), wrapper).unwrap();
    fs::set_permissions(
        bin.join("ssh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let port = free_port();
    let stderr_file = setup.root.path().join("forward.stderr");
    let mut child = setup
        .spawn_cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["forward", &format!("18765:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr_file).unwrap())
        .spawn()
        .unwrap();
    let reconnected = eventually(Duration::from_secs(20), || {
        fs::read_to_string(&stderr_file)
            .unwrap_or_default()
            .contains("reconnected; forwarding")
    });
    let printed = fs::read_to_string(&stderr_file).unwrap_or_default();
    std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(reconnected, "{printed}");
    assert!(
        printed.contains("ended (exit 255); reconnecting"),
        "{printed}"
    );
    assert_eq!(status.code(), Some(143), "{printed}");
    assert_eq!(fs::read_to_string(&count).unwrap().trim(), "2", "{printed}");
}
