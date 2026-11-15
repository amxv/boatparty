//! Seeding through the binary: `load` and `cheers` seed a fresh member from
//! the "GitHub" remote (a local bare repository over file://), take the
//! token from `gh auth token` only when a seed is attempted, and never let
//! the token reach an argument list, output, state, runs or the sandbox.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use common::e2e::E2e;
use common::runner::{git, write};

const TOKEN: &str = "gho_e2eseedtoken_never_print_41c9";

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Push the project to a bare "GitHub", add spies in front of `git`, `ssh`
/// and a fake `gh`, and return the bare repository.
fn github(setup: &E2e) -> PathBuf {
    let root = setup.root.path();
    let repo = setup.repo();
    let bare = root.join("github.git");
    git(root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    write(&repo.join("big.txt"), &noise(64 * 1024));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "big"]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(
        &repo,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    );

    let bin = root.join("bin");
    let real_git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let argv = root.join("argv.log");
    let hashes = root.join("password-hashes.log");
    executable(
        &bin.join("git"),
        &format!(
            "#!/usr/bin/env bash\n\
             printf 'git %s\\n' \"$*\" >> '{argv}'\n\
             configs=()\n\
             args=(\"$@\")\n\
             i=0\n\
             while [ $i -lt ${{#args[@]}} ]; do\n\
               a=\"${{args[$i]}}\"\n\
               if [ \"$a\" = -c ]; then i=$((i+1)); configs+=(-c \"${{args[$i]}}\");\n\
               elif [ \"$a\" = fetch ]; then\n\
                 p=$(printf 'protocol=https\\nhost=github.com\\npath=a/b.git\\n\\n' | GIT_TERMINAL_PROMPT=0 '{real_git}' -C / \"${{configs[@]}}\" credential fill 2>/dev/null | sed -n 's/^password=//p')\n\
                 printf '%s' \"$p\" | '{real_git}' -C / hash-object --stdin >> '{hashes}'\n\
                 break\n\
               fi\n\
               i=$((i+1))\n\
             done\n\
             exec '{real_git}' \"$@\"\n",
            argv = argv.display(),
            hashes = hashes.display(),
        ),
    );
    // Record ssh's arguments, then behave as the harness's fake.
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
    bare
}

/// Bytes that barely compress.
fn noise(len: usize) -> String {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
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

#[test]
fn a_fresh_member_is_seeded_with_a_gh_token_that_never_leaks() {
    let setup = E2e::with_history("seed-e2e", None);
    let boat = setup.boat();
    github(&setup);
    let seeded = |command: &mut assert_cmd::Command| {
        command
            .env("FAKE_SSH_CHECK", "0")
            .env("BOATPARTY_SEED_ALLOW_FILE", "1")
            .env("BOATPARTY_RUNNER_SEED_FILE", "1")
            .env_remove("BOATPARTY_GITHUB_TOKEN");
    };
    setup.cmd(&boat).arg("launch").assert().success();
    write(&setup.repo().join("local.txt"), "unpushed\n");
    git(&setup.repo(), &["add", "-A"]);
    git(&setup.repo(), &["commit", "-qm", "unpushed"]);
    write(&setup.repo().join("a.txt"), "edited\n");

    let mut load = setup.cmd(&boat);
    seeded(&mut load);
    let output = load.args(["--json", "load"]).output().unwrap();
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["seed_route"], "github", "{doc}");
    assert_eq!(doc["seed_fallback"], Value::Null);
    assert!(doc["pack_bytes"].as_u64().unwrap() < 4096, "{doc}");
    assert_eq!(gh_calls(&setup), 1);
    let remote = setup.remote().join("app");
    assert_eq!(
        git(&remote, &["rev-parse", "HEAD"]),
        git(&setup.repo(), &["rev-parse", "HEAD"])
    );
    assert_eq!(git(&remote, &["log", "--oneline"]).lines().count(), 1);
    git(&remote, &["fsck", "--no-dangling", "--no-progress"]);
    let mut outputs = vec![stdout, stderr];

    // A warm run needs no seed, so gh is not asked again.
    write(&setup.repo().join("a.txt"), "edited twice\n");
    let mut cheers = setup.cmd(&boat);
    seeded(&mut cheers);
    let output = cheers
        .args(["--json", "cheers", "--", "git", "log", "--oneline"])
        .output()
        .unwrap();
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let result: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(result["seed_route"], "none", "{result}");
    assert_eq!(gh_calls(&setup), 1);
    outputs.extend([stdout, stderr]);

    // The token went to Git's credential helper once ...
    let hashes = fs::read_to_string(setup.root.path().join("password-hashes.log")).unwrap();
    assert_eq!(hashes.lines().count(), 1);
    assert_eq!(hashes.trim(), git_hash(TOKEN));
    // ... and nowhere else: no argument list of git, ssh or gh, no output,
    // nothing in boatparty's home (state.json, runs.jsonl, ssh configs) or
    // on the "sandbox".
    let argv = fs::read_to_string(setup.root.path().join("argv.log")).unwrap();
    assert!(argv.contains("git ") && argv.contains("ssh ") && argv.contains("gh auth token"));
    assert!(!argv.contains(TOKEN));
    for output in &outputs {
        assert!(!output.contains(TOKEN));
    }
    assert_no_token_under(&setup.home());
    assert_no_token_under(&setup.remote());
    assert!(
        !fs::read_to_string(setup.events())
            .unwrap_or_default()
            .contains(TOKEN)
    );
    let runs = fs::read_to_string(setup.home().join("state/runs.jsonl")).unwrap();
    assert!(runs.contains("\"seed_route\":\"none\""), "{runs}");
}

fn git_hash(text: &str) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["-C", "/", "hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    text_of(child.wait_with_output().unwrap().stdout)
}

fn text_of(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_string()
}

#[test]
fn seeding_off_or_token_off_in_the_project_file_is_honored() {
    let setup = E2e::with_history("seed-e2e-off", None);
    let boat = setup.boat();
    let bare = github(&setup);
    let file = setup.repo().join("boatparty.toml");
    let base = fs::read_to_string(&file).unwrap();

    // github_token = false: seeded anonymously, gh never runs.
    fs::write(&file, format!("{base}\n[sync]\ngithub_token = false\n")).unwrap();
    git(&setup.repo(), &["add", "-A"]);
    git(&setup.repo(), &["commit", "-qm", "token off"]);
    git(&setup.repo(), &["push", "-q", "origin", "main"]);
    git(&setup.repo(), &["fetch", "-q", "origin"]);
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SEED_ALLOW_FILE", "1")
        .env("BOATPARTY_RUNNER_SEED_FILE", "1")
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let doc: Value = serde_json::from_str(&text(&output.stdout)).unwrap();
    assert_eq!(doc["seed_route"], "github", "{doc}");
    assert_eq!(gh_calls(&setup), 0);
    let hashes = fs::read_to_string(setup.root.path().join("password-hashes.log")).unwrap();
    assert_eq!(hashes.trim(), git_hash(""));

    // seed = false on a fresh member: the full pack, no gh, no fetch.
    fs::write(&file, format!("{base}\n[sync]\nseed = false\n")).unwrap();
    fs::remove_dir_all(setup.remote().join("app")).unwrap();
    let state = setup.home().join("state/state.json");
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    doc["projects"]["app"]["members"][0]["sync"]["known"] = Value::Array(Vec::new());
    fs::write(&state, doc.to_string()).unwrap();
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .env("BOATPARTY_SEED_ALLOW_FILE", "1")
        .env("BOATPARTY_RUNNER_SEED_FILE", "1")
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let doc: Value = serde_json::from_str(&text(&output.stdout)).unwrap();
    assert_eq!(doc["seed_route"], "none");
    assert!(
        doc["seed_skipped"]
            .as_str()
            .unwrap()
            .contains("seed = false")
    );
    assert!(doc["pack_bytes"].as_u64().unwrap() > 8 * 1024, "{doc}");
    assert_eq!(gh_calls(&setup), 0);
    assert!(bare.exists());
}
