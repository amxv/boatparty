//! Project configuration end to end: `init` stack detection on fixture
//! projects, `config show|validate`, `preset list|show|eject`, and the
//! named form of `cheers`.

mod common;

use std::fs;
use std::path::Path;

use common::e2e::E2e;
use common::runner::git;
use common::{TempDir, boatparty};
use serde_json::Value;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/init");

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "boatparty.toml.expected" {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A Git repository at `<root>/app` holding a copy of the fixture `stack`.
fn repo(root: &TempDir, stack: Option<&str>) -> std::path::PathBuf {
    let repo = root.path().join("app");
    match stack {
        Some(stack) => copy_dir(&Path::new(FIXTURES).join(stack), &repo),
        None => fs::create_dir_all(&repo).unwrap(),
    }
    git(&repo, &["init", "-q"]);
    repo
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn init_turns_each_fixture_stack_into_the_expected_file() {
    for stack in ["rust", "node", "python", "go"] {
        let root = TempDir::new(&format!("init-{stack}"));
        let repo = repo(&root, Some(stack));
        let home = root.path().join("home");
        let output = boatparty(&home)
            .current_dir(&repo)
            .args(["--json", "init", "--name", "app"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{stack}: {output:?}");
        let expected = fs::read_to_string(
            Path::new(FIXTURES)
                .join(stack)
                .join("boatparty.toml.expected"),
        )
        .unwrap();
        let written = fs::read_to_string(repo.join("boatparty.toml")).unwrap();
        assert_eq!(written, expected, "{stack}");
        let document = json(&output);
        assert_eq!(document["presets"][0], stack, "{stack}");
        assert!(document["commands"]["test"].is_string(), "{stack}");

        // The written file is valid and resolves with the preset applied.
        let shown = boatparty(&home)
            .current_dir(&repo)
            .args(["--json", "config", "show", "--resolved"])
            .output()
            .unwrap();
        assert!(shown.status.success(), "{stack}: {shown:?}");
        let shown = json(&shown);
        assert_eq!(shown["presets"][0]["name"], stack);
        assert_eq!(shown["presets"][0]["source"], "built-in");
        assert!(!shown["config"]["ensure"].as_array().unwrap().is_empty());
        boatparty(&home)
            .current_dir(&repo)
            .args(["config", "validate"])
            .assert()
            .success();
    }
}

#[test]
fn init_without_a_known_stack_writes_an_empty_presets_list() {
    let root = TempDir::new("init-none");
    let repo = repo(&root, None);
    let home = root.path().join("home");
    boatparty(&home)
        .current_dir(&repo)
        .args(["init", "--name", "app"])
        .assert()
        .success();
    let written = fs::read_to_string(repo.join("boatparty.toml")).unwrap();
    assert!(written.contains("presets = []\n"), "{written}");
    assert!(!written.contains("[commands]"), "{written}");
}

#[test]
fn an_ejected_and_edited_preset_changes_the_resolved_configuration() {
    let root = TempDir::new("eject");
    let repo = repo(&root, Some("go"));
    let home = root.path().join("home");
    let run = |args: &[&str]| {
        boatparty(&home)
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["init", "--name", "app"]).status.success());
    let before = json(&run(&[
        "--json",
        "config",
        "show",
        "--resolved",
        "--origin",
    ]));
    assert_eq!(
        before["config"]["run"]["env"]["GOCACHE"],
        "/home/user/.boatparty/cache/go-build"
    );
    assert_eq!(before["origins"]["run.env.GOCACHE"], "preset go (built-in)");

    let ejected = run(&["--json", "preset", "eject", "go"]);
    assert!(ejected.status.success(), "{ejected:?}");
    let path = fs::canonicalize(&repo)
        .unwrap()
        .join(".boatparty/presets/go.toml");
    assert_eq!(json(&ejected)["path"], path.display().to_string());
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("preset\n# eject`"), "{text}");
    fs::write(
        &path,
        text.replace(
            "/home/user/.boatparty/cache/go-build",
            "/home/user/go-cache",
        ),
    )
    .unwrap();

    let after = json(&run(&[
        "--json",
        "config",
        "show",
        "--resolved",
        "--origin",
    ]));
    assert_eq!(
        after["config"]["run"]["env"]["GOCACHE"],
        "/home/user/go-cache"
    );
    assert_eq!(after["presets"][0]["source"], "project");
    let origin = after["origins"]["run.env.GOCACHE"].as_str().unwrap();
    assert!(origin.starts_with("preset go (project: "), "{origin}");
    let text_view = run(&["config", "show", "--resolved", "--origin"]);
    let text_view = String::from_utf8_lossy(&text_view.stdout).to_string();
    assert!(
        text_view.contains("GOCACHE = \"/home/user/go-cache\"  # preset go (project: "),
        "{text_view}"
    );

    // A second eject refuses to overwrite the edited copy.
    let again = run(&["preset", "eject", "go"]);
    assert_eq!(again.status.code(), Some(125));
    let listed = json(&run(&["--json", "preset", "list"]));
    let go = listed["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "go")
        .unwrap()
        .clone();
    assert_eq!(go["source"], "project");
    assert_eq!(go["used"], true);
    assert_eq!(go["shadows"][0], "built-in");
    let shown = json(&run(&["--json", "preset", "show", "go"]));
    assert!(
        shown["text"]
            .as_str()
            .unwrap()
            .contains("/home/user/go-cache")
    );

    // An edit with an unknown key is caught by validate, naming the key.
    fs::write(&path, "[run]\nscritp = \"x\"\n").unwrap();
    let invalid = run(&["config", "validate"]);
    assert_eq!(invalid.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("unknown key run.scritp"), "{stderr}");
}

#[test]
fn a_user_preset_applies_to_every_project_and_the_project_file_overrides_it() {
    let root = TempDir::new("user-preset");
    let repo = repo(&root, None);
    let home = root.path().join("home");
    let user_dir = home.join("config/presets");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        user_dir.join("team.toml"),
        "# Team defaults.\n[sync]\nhistory = \"full\"\n[commands]\nlint = \"make lint\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("boatparty.toml"),
        "version = 1\n[project]\nname = \"app\"\npresets = [\"team\"]\n\
         [commands]\nlint = \"make lint-fast\"\n",
    )
    .unwrap();
    let shown = boatparty(&home)
        .current_dir(&repo)
        .args(["--json", "config", "show", "--resolved", "--origin"])
        .output()
        .unwrap();
    let shown = json(&shown);
    assert_eq!(shown["config"]["sync"]["history"], "full");
    assert_eq!(shown["config"]["commands"]["lint"], "make lint-fast");
    assert_eq!(shown["origins"]["commands.lint"], "boatparty.toml");
    let own = json(
        &boatparty(&home)
            .current_dir(&repo)
            .args(["--json", "config", "show"])
            .output()
            .unwrap(),
    );
    assert_eq!(own["view"], "file");
    assert!(own["config"].get("sync").is_none());
}

// ----- `cheers <name>` -------------------------------------------------------

fn append(setup: &E2e, text: &str) {
    let file = setup.repo().join("boatparty.toml");
    let mut current = fs::read_to_string(&file).unwrap();
    current.push_str(text);
    fs::write(&file, current).unwrap();
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn cheers_name_prefers_commands_then_the_script_runner_then_fails_listing_names() {
    let setup = E2e::new("cheers-named");
    append(
        &setup,
        "\n[commands]\ngreet = \"printf 'hello %s\\\\n'\"\nlint = \"true\"\n",
    );
    let boat = setup.boat();
    setup.launch(&boat);

    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "greet", "big world"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "hello big world\n");

    // Unknown name, no script runner: fails before anything remote runs.
    let _ = fs::remove_file(setup.events());
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "build"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no command named \"build\""), "{stderr}");
    assert!(stderr.contains("greet, lint"), "{stderr}");
    assert!(
        !setup.event_lines().iter().any(|line| line == "ssh-run"),
        "{:?}",
        setup.event_lines()
    );

    // With a script runner the name falls through to it; [commands] still wins.
    append(&setup, "\n[run]\nscript = \"echo runner\"\n");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "build", "--release"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output), "runner build --release\n");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "greet", "again"])
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "hello again\n");

    let log = setup.cmd(&boat).args(["log", "--json"]).output().unwrap();
    let runs = json(&log)["runs"].as_array().unwrap().clone();
    let last = runs.last().unwrap();
    assert_eq!(last["name"], "greet");
    assert_eq!(last["command"], "printf 'hello %s\\n' again");
}

#[test]
fn copy_back_off_leaves_the_local_tree_alone() {
    let setup = E2e::new("copy-back-off");
    append(&setup, "copy_back = false\n");
    let boat = setup.boat();
    setup.launch(&boat);
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "cheers", "--sh", "printf 'x\\n' >> a.txt"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let frames: Vec<Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(frames.last().unwrap()["copy_back"]["status"], "off");
    assert_eq!(
        fs::read_to_string(setup.repo().join("a.txt")).unwrap(),
        "one\n"
    );
}
