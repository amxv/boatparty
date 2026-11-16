//! Build credentials through the binary: `run` sends the local Git
//! identity unless `[run] git_identity = false`, and runs `gh auth token`
//! only when `[run] github_token = true`, after which the token reaches
//! Git's credential helper in the command and nothing else: no argument
//! list, output, state, runs or file on the "sandbox".

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use common::e2e::E2e;
use common::runner::git;

const TOKEN: &str = "gho_e2ebuildtoken_never_print_5d1e";

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Spies in front of `ssh` and a fake `gh` that counts its calls.
fn spies(setup: &E2e) {
    let root = setup.root.path();
    let bin = root.join("bin");
    let argv = root.join("argv.log");
    let ssh = bin.join("ssh");
    let fake = fs::read_to_string(&ssh).unwrap();
    let body = fake.replacen(
        "#!/bin/sh\n",
        &format!(
            "#!/bin/sh\nprintf 'ssh %s\\n' \"$*\" >> '{}'\n",
            argv.display()
        ),
        1,
    );
    executable(&ssh, &body);
    executable(
        &bin.join("gh"),
        &format!(
            "#!/bin/sh\nprintf 'gh %s\\n' \"$*\" >> '{argv}'\n\
             echo called >> '{calls}'\n\
             if [ \"$1 $2\" = 'auth token' ]; then echo {TOKEN}; fi\n",
            argv = argv.display(),
            calls = root.join("gh-calls.log").display(),
        ),
    );
}

fn gh_calls(setup: &E2e) -> usize {
    fs::read_to_string(setup.root.path().join("gh-calls.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn assert_no_token_under(dir: &Path) {
    for file in files_under(dir) {
        let bytes = fs::read(&file).unwrap_or_default();
        assert!(
            !text(&bytes).contains(TOKEN),
            "the token is in {}",
            file.display()
        );
    }
}

fn git_hash(value: &str) -> String {
    use std::io::Write;
    let mut child = common::isolated("git")
        .args(["-C", "/", "hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(value.as_bytes())
        .unwrap();
    text(&child.wait_with_output().unwrap().stdout)
        .trim()
        .to_string()
}

/// `run --json --sh <line>`: the exit code, the command's stdout, the
/// result frame and everything printed.
fn run(setup: &E2e, boat: &common::FakeBoat, line: &str) -> (i32, String, Value, String) {
    let shm = setup.root.path().join("shm");
    fs::create_dir_all(&shm).unwrap();
    let output = setup
        .cmd(boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_RUNNER_SHM", &shm)
        .env_remove("BOATPARTY_GITHUB_TOKEN")
        .args(["--json", "run", "--sh", line])
        .output()
        .unwrap();
    let stdout = text(&output.stdout);
    let printed = format!("{stdout}{}", text(&output.stderr));
    let mut command_out = String::new();
    let mut result = Value::Null;
    for frame in stdout.lines() {
        let frame: Value = serde_json::from_str(frame).unwrap();
        match frame["type"].as_str() {
            Some("stdout") => command_out.push_str(frame["data"].as_str().unwrap_or_default()),
            Some("result") => result = frame,
            _ => {}
        }
    }
    (
        output.status.code().unwrap_or(-1),
        command_out,
        result,
        printed,
    )
}

fn set_run(setup: &E2e, base: &str, run: &str) {
    fs::write(
        setup.repo().join("boatparty.toml"),
        format!("{base}\n[run]\n{run}\n"),
    )
    .unwrap();
}

#[test]
fn identity_and_token_reach_the_command_only_as_configured() {
    let setup = E2e::with_history("cred-e2e", None);
    let boat = setup.boat();
    spies(&setup);
    let file = setup.repo().join("boatparty.toml");
    let base = fs::read_to_string(&file).unwrap();
    git(&setup.repo(), &["config", "user.name", "Ada Lovelace"]);
    git(&setup.repo(), &["config", "user.email", "ada@example.com"]);
    setup.start(&boat);

    // Defaults: the identity goes, no token is fetched.
    let commit =
        "git commit --allow-empty -qm remote && git log -1 --format='%an <%ae> / %cn <%ce>'";
    let (code, out, result, printed) = run(&setup, &boat, commit);
    assert_eq!(code, 0, "{printed}");
    assert_eq!(
        out,
        "Ada Lovelace <ada@example.com> / Ada Lovelace <ada@example.com>\n"
    );
    assert_eq!(result["git_identity"], true, "{result}");
    assert_eq!(result["github_token"], false, "{result}");
    assert_eq!(gh_calls(&setup), 0);
    let mut outputs = vec![printed];

    // git_identity = false: no identity of the developer's.
    set_run(&setup, &base, "git_identity = false");
    let (_, out, result, printed) = run(&setup, &boat, commit);
    assert!(!out.contains("Ada"), "{out}");
    assert_eq!(result["git_identity"], false, "{result}");
    assert_eq!(gh_calls(&setup), 0);
    outputs.push(printed);

    // github_token = true: gh runs once per command, and Git in the command
    // gets the token for github.com.
    set_run(&setup, &base, "github_token = true");
    let fill = "printf 'protocol=https\\nhost=github.com\\n\\n' | git credential fill \
                | sed -n 's/^password=//p' | git hash-object --stdin";
    let (code, out, result, printed) = run(&setup, &boat, fill);
    assert_eq!(code, 0, "{printed}");
    assert_eq!(out.trim(), git_hash(&format!("{TOKEN}\n")), "{printed}");
    assert_eq!(result["github_token"], true, "{result}");
    assert_eq!(gh_calls(&setup), 1);
    outputs.push(printed);

    // Nowhere else: no argument list of ssh or gh, no output, nothing in
    // boatparty's home (state.json, runs.jsonl, ssh configs) or on the
    // "sandbox" (checkout, state, RAM directory).
    let argv = fs::read_to_string(setup.root.path().join("argv.log")).unwrap();
    assert!(argv.contains("ssh ") && argv.contains("gh auth token"));
    assert!(!argv.contains(TOKEN));
    for output in &outputs {
        assert!(!output.contains(TOKEN));
    }
    assert_no_token_under(&setup.home());
    assert_no_token_under(&setup.remote());
    assert_no_token_under(&setup.root.path().join("shm"));
    assert!(
        !fs::read_to_string(setup.events())
            .unwrap_or_default()
            .contains(TOKEN)
    );
    let runs = fs::read_to_string(setup.home().join("state/runs.jsonl")).unwrap();
    assert!(runs.contains("\"github_token\":true"), "{runs}");
}

/// `shell --json -- sh -c <line>`: the exit code, the command's stdout, the
/// result frame and everything printed.
fn shell(setup: &E2e, boat: &common::FakeBoat, line: &str) -> (i32, String, Value, String) {
    let shm = setup.root.path().join("shm");
    fs::create_dir_all(&shm).unwrap();
    let output = setup
        .cmd(boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_RUNNER_SHM", &shm)
        .env_remove("BOATPARTY_GITHUB_TOKEN")
        .args(["--json", "shell", "--", "sh", "-c", line])
        .output()
        .unwrap();
    let stdout = text(&output.stdout);
    let printed = format!("{stdout}{}", text(&output.stderr));
    let mut command_out = String::new();
    let mut result = Value::Null;
    for frame in stdout.lines() {
        let frame: Value = serde_json::from_str(frame).unwrap();
        match frame["type"].as_str() {
            Some("stdout") => command_out.push_str(frame["data"].as_str().unwrap_or_default()),
            Some("result") => result = frame,
            _ => {}
        }
    }
    (
        output.status.code().unwrap_or(-1),
        command_out,
        result,
        printed,
    )
}

#[test]
fn board_gets_the_identity_the_token_and_the_environment_like_cheers() {
    let setup = E2e::with_history("cred-shell", None);
    let boat = setup.boat();
    spies(&setup);
    let file = setup.repo().join("boatparty.toml");
    let base = fs::read_to_string(&file).unwrap();
    git(&setup.repo(), &["config", "user.name", "Ada Lovelace"]);
    git(&setup.repo(), &["config", "user.email", "ada@example.com"]);
    setup.start(&boat);

    // The identity, [run.env] and BOATPARTY=1, in the checkout; no token.
    fs::write(
        &file,
        format!("{base}\n[run.env]\nGREETING = \"hi from $BOATPARTY_TEST_HOME_HINT\"\n"),
    )
    .unwrap();
    let show = "printf '%s|%s|%s|%s|%s\\n' \"$GIT_AUTHOR_NAME\" \"$(git config user.email)\" \
                \"$BOATPARTY\" \"$GREETING\" \"$(basename \"$PWD\")\"";
    let (code, out, result, printed) = shell(&setup, &boat, &format!("{show}; exit 3"));
    assert_eq!(code, 3, "{printed}");
    assert_eq!(
        out, "Ada Lovelace|ada@example.com|1|hi from |app\n",
        "{printed}"
    );
    assert_eq!(result["git_identity"], true, "{result}");
    assert_eq!(result["github_token"], false, "{result}");
    assert_eq!(gh_calls(&setup), 0);
    let mut outputs = vec![printed];

    // With github_token = true, Git in the command gets the token, and the
    // server is gone once shell returns.
    set_run(&setup, &base, "github_token = true");
    let fill = "printf 'protocol=https\\nhost=github.com\\n\\n' | git credential fill \
                | sed -n 's/^password=//p' | git hash-object --stdin";
    let (code, out, result, printed) = shell(&setup, &boat, fill);
    assert_eq!(code, 0, "{printed}");
    assert_eq!(out.trim(), git_hash(&format!("{TOKEN}\n")), "{printed}");
    assert_eq!(result["github_token"], true, "{result}");
    assert_eq!(gh_calls(&setup), 1);
    outputs.push(printed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline
        && files_under(&setup.root.path().join("shm"))
            .iter()
            .any(|path| path.to_string_lossy().contains("bpc-"))
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !files_under(&setup.root.path().join("shm"))
            .iter()
            .any(|path| path.to_string_lossy().contains("bpc-")),
        "the token server outlived the shell command"
    );

    let argv = fs::read_to_string(setup.root.path().join("argv.log")).unwrap();
    assert!(argv.contains(" hold app "), "{argv}");
    assert!(!argv.contains(TOKEN));
    for output in &outputs {
        assert!(!output.contains(TOKEN));
    }
    assert_no_token_under(&setup.home());
    assert_no_token_under(&setup.remote());
    assert_no_token_under(&setup.root.path().join("shm"));
}
