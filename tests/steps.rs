//! The project's settings on the runner (the RunnerSpec): setup and ensure
//! steps, `[run.env]`, the RAM scratch directory, mtime flooring, directory
//! mtimes while hydrating, and the cache repair after a restore. The runner
//! runs locally against scratch repositories.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use boatparty::project::ProjectConfig;
use boatparty::runner::RunnerSpec;
use boatparty::sync::{self, History, MirrorRequest, RunEnd, RunOutcome, Sink, WorktreeInfo};
use common::TempDir;
use common::runner::{LocalRunner, base_repo, git, write};

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

struct Run {
    outcome: RunOutcome,
    stdout: String,
    stderr: String,
    events: Vec<String>,
}

impl Run {
    fn exit(&self) -> i32 {
        match self.outcome.end {
            RunEnd::Done { exit_code, .. } => exit_code,
            ref other => panic!("no result: {other:?}; stderr {}", self.stderr),
        }
    }
}

/// A scratch member: the runner run locally with the spec of `config`
/// (the body of a boatparty.toml after `[project]`).
struct Member {
    _root: TempDir,
    local: std::path::PathBuf,
    host: LocalRunner,
    known: Vec<String>,
}

impl Member {
    fn new(label: &str, config: &str) -> Self {
        let root = TempDir::new(label);
        let local = base_repo(root.path());
        let host = LocalRunner::new(root.path(), "remote");
        let text = format!("version = 1\n[project]\nname = \"app\"\n{config}");
        let project = ProjectConfig::parse(&text, Path::new("boatparty.toml")).unwrap();
        *host.spec.borrow_mut() = Some(RunnerSpec::new(project.config()));
        let shm = root.path().join("shm");
        fs::create_dir_all(&shm).unwrap();
        host.env
            .borrow_mut()
            .push(("BOATPARTY_RUNNER_SHM".into(), shm.display().to_string()));
        Self {
            _root: root,
            local,
            host,
            known: Vec::new(),
        }
    }

    fn root(&self) -> &Path {
        self.local.parent().unwrap()
    }

    fn remote(&self) -> &Path {
        &self.host.repo
    }

    fn set_env(&self, name: &str, value: &str) {
        let mut env = self.host.env.borrow_mut();
        env.retain(|(key, _)| key != name);
        env.push((name.into(), value.into()));
    }

    fn request<'a>(
        &'a self,
        info: &'a WorktreeInfo,
        snapshot: &'a sync::Snapshot,
        refs: &'a [sync::LocalRef],
    ) -> MirrorRequest<'a> {
        MirrorRequest {
            project: "app",
            info,
            snapshot,
            refs,
            send_refs: true,
            known: &self.known,
            history: History::Full,
            seed: None,
        }
    }

    fn run(&mut self, command: &str) -> Run {
        let info = WorktreeInfo::read(&self.local).unwrap();
        let snapshot = sync::snapshot(&info).unwrap();
        let refs = sync::local_refs(&info).unwrap();
        let request = self.request(&info, &snapshot, &refs);
        let mut printed = Collect::default();
        let outcome = sync::run_command(
            &self.host,
            &request,
            command,
            &mut printed,
            &sync::NeverCancel,
        )
        .unwrap();
        if !outcome.known.is_empty() {
            self.known = outcome.known.clone();
        }
        Run {
            outcome,
            stdout: String::from_utf8_lossy(&printed.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&printed.stderr).into_owned(),
            events: printed.events,
        }
    }

    fn load(&mut self) -> String {
        let info = WorktreeInfo::read(&self.local).unwrap();
        let snapshot = sync::snapshot(&info).unwrap();
        let refs = sync::local_refs(&info).unwrap();
        let request = self.request(&info, &snapshot, &refs);
        let mut log = Vec::new();
        let outcome = sync::mirror(&self.host, &request, &mut log).unwrap();
        self.known = outcome.known;
        String::from_utf8(log).unwrap()
    }
}

/// Lines steps appended to `<dir>/steps.log`.
fn step_log(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("steps.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A step line that records that it ran.
fn record(member_root: &Path, name: &str) -> String {
    format!(
        "echo {name} >> '{}'",
        member_root.join("steps.log").display()
    )
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

fn nanos(path: &Path) -> u32 {
    mtime(path)
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}

/// Give `path` an mtime with a fractional second.
fn set_fractional_mtime(path: &Path) {
    let when = SystemTime::now() - Duration::from_millis(2500);
    let when = UNIX_EPOCH
        + Duration::new(
            when.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            123_456_789,
        );
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn an_ensure_step_runs_once_until_its_key_file_changes_and_again_when_its_check_fails() {
    let root = TempDir::new("steps-key");
    let log = record(root.path(), "deps");
    let mut member = Member::new(
        "ensure",
        &format!(
            "[[ensure]]\nname = \"deps\"\nkey_files = [\"deps.lock\"]\n\
             check = \"test -f installed\"\nrun = \"{log}; touch installed; echo installing\"\n"
        ),
    );
    let ran = || step_log(root.path()).len();
    write(&member.local.join("deps.lock"), "v1\n");
    write(&member.local.join(".gitignore"), "ignored/\ninstalled\n");

    let first = member.run("echo out");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    assert_eq!(ran(), 1);
    // Step output goes to stderr; stdout is the command's alone.
    assert_eq!(first.stdout, "out\n");
    assert!(first.stderr.contains("installing"), "{}", first.stderr);
    assert!(
        first.stderr.contains("boatparty: ensure deps (first run)"),
        "{}",
        first.stderr
    );
    assert_eq!(first.events, ["preparing", "started"]);

    let second = member.run("true");
    assert_eq!(second.exit(), 0);
    assert_eq!(ran(), 1, "an unchanged key skips the step");
    assert_eq!(second.events, ["started"]);
    assert!(!second.stderr.contains("ensure"), "{}", second.stderr);

    write(&member.local.join("deps.lock"), "v2\n");
    let third = member.run("true");
    assert_eq!(ran(), 2);
    assert!(
        third.stderr.contains("(its key changed)"),
        "{}",
        third.stderr
    );

    fs::remove_file(member.remote().join("installed")).unwrap();
    let fourth = member.run("true");
    assert_eq!(ran(), 3);
    assert!(
        fourth.stderr.contains("(its check failed)"),
        "{}",
        fourth.stderr
    );
    member.run("true");
    assert_eq!(ran(), 3);
}

#[test]
fn a_key_command_and_a_step_directory_are_part_of_the_key() {
    let root = TempDir::new("steps-dir");
    let log = record(root.path(), "web");
    let version = root.path().join("version");
    write(&version, "1\n");
    let mut member = Member::new(
        "ensure-dir",
        &format!(
            "[[ensure]]\nname = \"web\"\ndir = \"web\"\nkey_files = [\"web.lock\"]\n\
             key_command = \"cat '{}'\"\nrun = \"{log}; pwd >&2\"\n\
             [[ensure]]\nname = \"absent\"\ndir = \"nowhere\"\nrun = \"exit 9\"\n",
            version.display()
        ),
    );
    write(&member.local.join("web/web.lock"), "a\n");
    let first = member.run("true");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    assert!(first.stderr.contains("/remote/web\n"), "{}", first.stderr);
    member.run("true");
    assert_eq!(step_log(root.path()), ["web"]);
    write(&version, "2\n");
    member.run("true");
    assert_eq!(step_log(root.path()), ["web", "web"]);
}

#[test]
fn setup_steps_run_once_per_boot_unless_their_check_succeeds() {
    let root = TempDir::new("setup");
    let mut member = Member::new(
        "setup",
        &format!(
            "[[setup]]\nname = \"needed\"\nrun = \"{}\"\n\
             [[setup]]\nname = \"satisfied\"\ncheck = \"true\"\nrun = \"{}\"\n",
            record(root.path(), "needed"),
            record(root.path(), "satisfied"),
        ),
    );
    let log = || step_log(root.path());
    let first = member.run("true");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    assert!(
        first
            .stderr
            .contains("boatparty: setup needed (the sandbox started)"),
        "{}",
        first.stderr
    );
    assert_eq!(log(), ["needed"], "a satisfied setup step does not run");
    member.run("true");
    assert_eq!(log(), ["needed"], "setup runs once per boot");
    // A bare load never runs steps, even on a new boot.
    member.set_env("BOATPARTY_RUNNER_BOOT_ID", "second-boot");
    member.load();
    assert_eq!(log(), ["needed"]);
    member.run("true");
    assert_eq!(log(), ["needed", "needed"], "a new boot runs setup again");
}

#[test]
fn a_failing_step_stops_the_command_and_reports_its_exit_code() {
    let root = TempDir::new("steps-fail");
    let marker = root.path().join("command-ran");
    let mut member = Member::new(
        "steps-fail",
        "[[ensure]]\nname = \"broken\"\nrun = \"echo nope >&2; exit 3\"\n",
    );
    let run = member.run(&format!("touch '{}'", marker.display()));
    assert_eq!(run.exit(), 3);
    assert!(!marker.exists(), "the command must not run");
    assert_eq!(run.events, ["preparing", "failed-step ensure.broken"]);
    assert!(
        run.stderr
            .contains("boatparty: ensure step broken failed (exit 3); the command did not run"),
        "{}",
        run.stderr
    );
    // Not recorded as done: it runs again.
    let again = member.run("true");
    assert_eq!(again.exit(), 3);
}

#[test]
fn run_env_reaches_the_command_expanded_in_the_checkout() {
    let mut member = Member::new(
        "env",
        "[run.env]\nTOOL_HOME = \"$PWD/.tools\"\nPATH = \"/opt/fake/bin:$PATH\"\n\
         QUOTED = 'say \"hi\" to $USER_NAME'\nUSER_NAME = \"crew\"\n",
    );
    let run =
        member.run("printf '%s\\n' \"$BOATPARTY\" \"$TOOL_HOME\" \"${PATH%%:*}\" \"$QUOTED\"");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    let remote = member.remote().display().to_string();
    // Exported in name order, so QUOTED sees USER_NAME only if it sorts
    // first; it does not, and stays empty there.
    assert_eq!(
        run.stdout,
        format!("1\n{remote}/.tools\n/opt/fake/bin\nsay \"hi\" to \n")
    );
}

#[test]
fn the_command_gets_a_private_ram_tmpdir_that_is_removed_afterwards() {
    let mut member = Member::new("tmp", "");
    let shm = member.root().join("shm");
    let run = member.run("echo \"$TMPDIR\"; touch \"$TMPDIR/scratch\"; ls \"$TMPDIR\"");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    let mut lines = run.stdout.lines();
    let tmp = lines.next().unwrap();
    // Compared canonically: on macOS /var is a link to /private/var.
    let scratch = Path::new(tmp);
    assert_eq!(
        fs::canonicalize(scratch.parent().unwrap()).unwrap(),
        fs::canonicalize(&shm).unwrap(),
        "{tmp}"
    );
    assert!(
        scratch
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("boatparty-"),
        "{tmp}"
    );
    assert_eq!(lines.next(), Some("scratch"));
    assert!(!Path::new(tmp).exists(), "the scratch directory is removed");
    assert_eq!(fs::read_dir(&shm).unwrap().count(), 0);

    let mut off = Member::new("tmp-off", "[run]\nram_tmp = false\n");
    off.set_env("TMPDIR", "/var/tmp");
    let run = off.run("echo \"$TMPDIR\"");
    assert_eq!(run.stdout, "/var/tmp\n");
}

#[test]
fn build_outputs_are_floored_but_sources_and_cache_directories_are_not() {
    let mut member = Member::new(
        "floor",
        "[build]\noutput_dirs = [\"out\", \"pkg/*/out\"]\ncache_dirs = [\"incremental\"]\n",
    );
    // A tracked file inside an output directory is a source.
    write(&member.local.join("out/tracked.txt"), "source\n");
    write(&member.local.join("pkg/a/src.txt"), "source\n");
    git(&member.local, &["add", "-A"]);
    git(&member.local, &["commit", "-qm", "sources"]);
    write(
        &member.local.join(".gitignore"),
        "ignored/\nout/*\n!out/tracked.txt\npkg/*/out/\n",
    );
    let first = member.run("true");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    let remote = member.remote().to_path_buf();
    set_fractional_mtime(&remote.join("out/tracked.txt"));
    set_fractional_mtime(&remote.join("pkg/a/src.txt"));

    let run = member.run(
        "mkdir -p out/debug/incremental/crate-1 pkg/a/out && \
         printf o > out/debug/app && printf i > out/debug/incremental/crate-1/q && \
         printf p > pkg/a/out/lib && touch out/tracked.txt pkg/a/src.txt",
    );
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    assert_eq!(
        nanos(&remote.join("out/debug/app")),
        0,
        "outputs are floored"
    );
    assert_eq!(
        nanos(&remote.join("pkg/a/out/lib")),
        0,
        "globs are followed"
    );
    for source in ["out/tracked.txt", "pkg/a/src.txt"] {
        // The command touched them; flooring must leave their times alone
        // (a whole-second time here would be a 1 in 10^9 coincidence).
        assert_ne!(nanos(&remote.join(source)), 0, "{source} was floored");
    }
    assert_ne!(
        nanos(&remote.join("out/debug/incremental/crate-1/q")),
        0,
        "cache directories are not floored"
    );
}

#[test]
fn a_bare_load_runs_no_steps_and_floors_nothing() {
    let root = TempDir::new("load-bare");
    let mut member = Member::new(
        "load-bare",
        &format!(
            "[[setup]]\nname = \"s\"\nrun = \"{}\"\n[[ensure]]\nname = \"e\"\nrun = \"{}\"\n\
             [build]\noutput_dirs = [\"out\"]\n",
            record(root.path(), "setup"),
            record(root.path(), "ensure"),
        ),
    );
    write(&member.local.join(".gitignore"), "ignored/\nout/\n");
    member.load();
    let output = member.remote().join("out/file");
    write(&output, "built\n");
    set_fractional_mtime(&output);
    write(&member.local.join("a.txt"), "changed\n");
    let log = member.load();
    assert!(log.is_empty(), "{log}");
    assert!(!root.path().join("steps.log").exists(), "no step ran");
    assert_ne!(nanos(&output), 0, "nothing was floored");
    assert!(!member.host.state.join("floor").exists());
    // The same spec on a command applies everything.
    let run = member.run("true");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    assert_eq!(
        fs::read_to_string(root.path().join("steps.log")).unwrap(),
        "setup\nensure\n"
    );
    assert_eq!(nanos(&output), 0);
}

#[test]
fn a_missing_spec_takes_the_exit_96_path_once() {
    let mut member = Member::new("spec-missing", "[run.env]\nGREETING = \"ahoy\"\n");
    let hash = member
        .host
        .spec
        .borrow()
        .as_ref()
        .unwrap()
        .hash()
        .to_string();
    let spec_file = member.host.state.join(format!("specs/{hash}.sh"));
    assert!(!spec_file.exists());
    let run = member.run("echo $GREETING");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    assert_eq!(run.stdout, "ahoy\n");
    assert!(run.outcome.installed);
    assert_eq!(member.host.installs.get(), 1);
    assert!(spec_file.exists());
    assert!(!run.stderr.contains("boatparty-runner-missing"));
    let spawned = member.host.spawned.borrow();
    assert_eq!(spawned.len(), 2, "one refused attempt, one retry");
    assert_eq!(spawned[0][9], hash, "the spec is named after the history");
    drop(spawned);

    // A load names the spec too, and a different spec is uploaded again.
    *member.host.spec.borrow_mut() = Some(RunnerSpec::from_text("bp_ram_tmp=0\n".into()));
    member.load();
    assert_eq!(member.host.installs.get(), 2);
}

#[test]
fn directory_mtimes_are_saved_and_restored_only_while_hydrating() {
    let mut member = Member::new("dirs", "");
    let first = member.run("true");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    let dir = member.remote().join("dir");
    let saved = mtime(&dir);
    let manifest = fs::read(member.host.state.join("dirs/app")).unwrap();
    let manifest = String::from_utf8_lossy(&manifest);
    assert!(manifest.contains("\tdir\0"), "{manifest:?}");
    assert!(manifest.contains("\t.\0"), "{manifest:?}");
    assert!(!manifest.contains(".git"), "{manifest:?}");
    assert!(!manifest.contains("ignored"), "{manifest:?}");

    // While hydrating, a restore moves directory times; the recorded time
    // comes back before the sync.
    let moved = SystemTime::now() + Duration::from_secs(3600);
    let set = |when: SystemTime| {
        fs::File::open(&dir).unwrap().set_modified(when).unwrap();
    };
    let secs = |when: SystemTime| when.duration_since(UNIX_EPOCH).unwrap().as_secs();
    member.set_env("BOATPARTY_RUNNER_HYDRATING", "1");
    set(moved);
    member.load();
    assert_eq!(secs(mtime(&dir)), secs(saved));
    // Once settled, directory times are real and are left alone.
    member.set_env("BOATPARTY_RUNNER_HYDRATING", "0");
    set(moved);
    member.load();
    assert_eq!(mtime(&dir), moved);
}

#[test]
fn an_unreadable_cache_entry_left_by_a_restore_is_dropped_whole() {
    // Root, or a user with CAP_DAC_OVERRIDE (as on some CI runners), opens
    // any file, so permissions cannot stand in for a broken one there.
    let probe = std::env::temp_dir().join(format!("boatparty-unreadable-{}", std::process::id()));
    fs::write(&probe, "x").unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let opens = fs::File::open(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    if opens {
        eprintln!("skipped: this user opens files with mode 000");
        return;
    }
    let mut member = Member::new(
        "heal",
        "[build]\noutput_dirs = [\"out\"]\ncache_dirs = [\"incremental\"]\n",
    );
    write(&member.local.join(".gitignore"), "ignored/\nout/\n");
    member.run("true");
    let cache = member.remote().join("out/debug/incremental");
    write(&cache.join("good-1/s-1/a.o"), "a");
    write(&cache.join("bad-2/s-1/b.o"), "b");
    write(&cache.join("bad-2/s-1/c.o"), "c");
    let broken = cache.join("bad-2/s-1/b.o");
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o000)).unwrap();
    // Settled: checked once per boot, so a new boot checks again.
    member.set_env("BOATPARTY_RUNNER_BOOT_ID", "restored");
    let run = member.run("true");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("boatparty: dropping out/debug/incremental/bad-2"),
        "{}",
        run.stderr
    );
    assert!(!cache.join("bad-2").exists());
    assert!(cache.join("good-1/s-1/a.o").exists());

    // While hydrating: before every command.
    member.set_env("BOATPARTY_RUNNER_HYDRATING", "1");
    for round in 0..2 {
        let broken = cache.join(format!("bad-{round}/s-1/x.o"));
        write(&broken, "x");
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o000)).unwrap();
        let run = member.run("true");
        assert_eq!(run.exit(), 0, "{}", run.stderr);
        assert!(
            !cache.join(format!("bad-{round}")).exists(),
            "round {round}"
        );
    }
    assert!(cache.join("good-1/s-1/a.o").exists());
}

#[test]
fn files_exposed_by_changed_ignore_rules_are_removed_not_sent_back() {
    // A member moving between repositories keeps the last one's ignored
    // build output; under the next repository's rules it is untracked.
    let mut member = Member::new("ignore-change", "");
    write(&member.local.join(".gitignore"), "ignored/\nbuild/\n");
    let first = member.run("mkdir -p build && echo out > build/artifact");
    assert_eq!(first.exit(), 0, "{}", first.stderr);
    assert!(member.remote().join("build/artifact").exists());

    write(&member.local.join(".gitignore"), "ignored/\n");
    let run = member.run("true");
    assert_eq!(run.exit(), 0, "{}", run.stderr);
    match run.outcome.end {
        RunEnd::Done { patch_bytes, .. } => assert_eq!(patch_bytes, 0),
        ref other => panic!("{other:?}"),
    }
    assert!(!member.remote().join("build").exists());
    assert!(!member.local.join("build").exists());
}
