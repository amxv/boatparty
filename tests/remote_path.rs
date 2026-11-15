//! `[sandbox] remote_path`: a checkout somewhere other than
//! /home/user/<project>, honored by `load`, `cheers`, `board` and `fetch`.

mod common;

use std::fs;

use serde_json::Value;

use common::e2e::E2e;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn every_command_uses_the_configured_checkout() {
    let setup = E2e::new("remote-path");
    let boat = setup.boat();
    let file = setup.repo().join("boatparty.toml");
    let mut config = fs::read_to_string(&file).unwrap();
    config.push_str("\n[sandbox]\nremote_path = \"/home/user/work/app-checkout/\"\n");
    fs::write(&file, config).unwrap();
    setup.launch(&boat);
    let checkout = setup.remote().join("work/app-checkout");

    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["--json", "load"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["checkout"], "/home/user/work/app-checkout");
    assert!(checkout.join(".git").is_dir());
    assert!(checkout.join("boatparty.toml").is_file());
    assert!(!setup.remote().join("app/.git").exists());

    // cheers runs there, and its changes come back.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["cheers", "--sh", "pwd; echo made > made.txt"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout)
            .trim_end()
            .ends_with("work/app-checkout"),
        "{}",
        text(&output.stdout)
    );
    assert_eq!(
        fs::read_to_string(setup.repo().join("made.txt")).unwrap(),
        "made\n"
    );

    // board and fetch find the same directory; the command does not see
    // the runner's checkout variable.
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args([
            "board",
            "--",
            "sh",
            "-c",
            "pwd; echo \"repo=${BOATPARTY_RUNNER_REPO:-unset}\"",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("work/app-checkout\n"), "{stdout}");
    assert!(stdout.contains("repo=unset"), "{stdout}");

    fs::write(checkout.join("artifact.txt"), "built\n").unwrap();
    let dest = setup.root.path().join("artifact-copy.txt");
    let output = setup
        .cmd(&boat)
        .env("FAKE_SSH_CHECK", "0")
        .args(["fetch", "artifact.txt"])
        .arg(&dest)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(fs::read_to_string(&dest).unwrap(), "built\n");
}
