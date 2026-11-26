mod common;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use common::{FakeBoat, Reply, TEST_KEY, TempDir, boatparty, fake_tools, fixture, store_key};
use predicates::prelude::*;
use serde_json::Value;

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).expect("stdout is one JSON document")
}

fn assert_no_key(output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(TEST_KEY),
        "key printed on stdout: {stdout}"
    );
    assert!(
        !stderr.contains(TEST_KEY),
        "key printed on stderr: {stderr}"
    );
}

/// A fake that answers `GET /limits` with the recorded fixture and every
/// probe the way boat does for a key holding all actions.
fn healthy_boat() -> FakeBoat {
    let limits = fixture("limits.json");
    let not_found = fixture("errors/404-not-found.json");
    let invalid_type = fixture("errors/400-invalid-type.json");
    FakeBoat::start(
        move |request| match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/limits") => Reply::json(200, &limits),
            ("POST", "/sandboxes") => Reply::json(400, &invalid_type),
            _ => Reply::json(404, &not_found),
        },
    )
}

#[test]
fn help_lists_the_commands_and_the_exit_status() {
    let home = TempDir::new("help");
    boatparty(home.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("login"))
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("limits"))
        .stdout(predicate::str::contains("125"));
}

#[test]
fn shows_version() {
    let home = TempDir::new("version");
    boatparty(home.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn usage_errors_exit_2() {
    let home = TempDir::new("usage");
    boatparty(home.path())
        .arg("no-such-command")
        .assert()
        .code(2);
}

#[test]
fn login_never_takes_the_key_as_an_argument() {
    let home = TempDir::new("login-arg");
    boatparty(home.path())
        .args(["login", TEST_KEY])
        .assert()
        .code(2);
    boatparty(home.path())
        .args(["login", "--key-stdin", TEST_KEY])
        .write_stdin("")
        .assert()
        .code(2);
    // --key-stdin is required.
    boatparty(home.path()).arg("login").assert().code(2);
    assert!(!home.path().join("config").join("credentials.toml").exists());
}

#[test]
fn login_verifies_and_stores_the_key_with_mode_0600() {
    let home = TempDir::new("login");
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .args(["--json", "login", "--key-stdin"])
        .write_stdin(format!("  {TEST_KEY}\n"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    assert_eq!(document["ok"], true);
    assert_eq!(document["verified"], true);

    let path = home.path().join("config").join("credentials.toml");
    assert_eq!(document["credentials_file"], path.display().to_string());
    #[cfg(unix)]
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    #[cfg(unix)]
    assert_eq!(mode, 0o600);
    assert!(std::fs::read_to_string(&path).unwrap().contains(TEST_KEY));

    let requests = boat.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/limits");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some(format!("Bearer {TEST_KEY}").as_str())
    );
}

#[test]
fn login_refuses_a_key_the_provider_rejects() {
    let home = TempDir::new("login-401");
    let unauthorized = fixture("errors/401-unauthorized.json");
    let boat = FakeBoat::start(move |_| Reply::json(401, &unauthorized));
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .args(["--json", "login", "--key-stdin"])
        .write_stdin(TEST_KEY)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    assert_eq!(document["ok"], false);
    assert_eq!(document["error"]["code"], "credentials");
    assert!(!home.path().join("config").join("credentials.toml").exists());
}

#[test]
fn login_rejects_input_that_is_not_a_key_without_echoing_it() {
    let home = TempDir::new("login-bad");
    let output = boatparty(home.path())
        .args(["login", "--key-stdin", "--no-verify"])
        .write_stdin("sk-not-a-boat-key-SECRETPART")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("boatparty: error: "), "{stderr}");
    assert!(!stderr.contains("SECRETPART"), "{stderr}");
}

#[test]
fn first_use_creates_account_toml_with_defaults() {
    let home = TempDir::new("account");
    store_key(home.path());
    let account = home.path().join("config/account.toml");
    let text = std::fs::read_to_string(&account).unwrap();
    assert!(text.contains("version = 1"));
    assert!(text.contains("max_sandboxes = 8"));
    assert!(text.contains("idle_minutes = 5"));

    // An existing file is left alone.
    std::fs::write(&account, "version = 1\n[defaults]\nidle_minutes = 7\n").unwrap();
    store_key(home.path());
    assert_eq!(
        std::fs::read_to_string(&account).unwrap(),
        "version = 1\n[defaults]\nidle_minutes = 7\n"
    );
}

#[test]
fn a_bad_account_file_fails_with_its_path() {
    let home = TempDir::new("account-bad");
    std::fs::create_dir_all(home.path().join("config")).unwrap();
    std::fs::write(
        home.path().join("config/account.toml"),
        "version = 1\ntypo = 3\n",
    )
    .unwrap();
    let output = boatparty(home.path())
        .args(["--json", "limits"])
        .env("BOATPARTY_BOAT_API_KEY", TEST_KEY)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let document = parse_json(&output.stdout);
    assert_eq!(document["error"]["code"], "config");
    let message = document["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("account.toml") && message.contains("typo"),
        "{message}"
    );
}

#[test]
fn limits_json_reports_the_provider_figures() {
    let home = TempDir::new("limits");
    store_key(home.path());
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .args(["limits", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    let recorded: Value = serde_json::from_str(&fixture("limits.json")).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["provider"], "boat");
    for window in ["minute", "hour", "day"] {
        for field in ["limit", "used", "remaining"] {
            assert_eq!(
                document["starts"][window][field], recorded["starts"][window][field],
                "{window}.{field}"
            );
        }
    }
    assert_eq!(document["sandboxes"]["running"], 2);
    assert_eq!(document["sandboxes"]["max_running"], 100);
    assert_eq!(document["included_vm_time"]["hours_remaining"], 450.5);
    assert_eq!(
        document["included_vm_time"]["period_end"],
        "2026-12-01T00:00:00.000Z"
    );
    assert_eq!(document["sizes"][3]["size"], "xlarge");
    assert_eq!(document["sizes"][3]["available"], false);
    assert_eq!(document["can_start"], true);
}

#[test]
fn limits_text_shows_each_window() {
    let home = TempDir::new("limits-text");
    store_key(home.path());
    let boat = healthy_boat();
    boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .arg("limits")
        .assert()
        .success()
        .stdout(predicate::str::contains("hour"))
        .stdout(predicate::str::contains("2 running of at most 100"))
        .stdout(predicate::str::contains(
            "450.5 of 555.6 hours left, until 2026-12-01",
        ))
        .stdout(predicate::str::contains(
            "xlarge (needs the $100/month plan or higher)",
        ));
}

#[test]
fn environment_key_overrides_the_stored_key() {
    let home = TempDir::new("env-key");
    store_key(home.path());
    let boat = healthy_boat();
    let env_key = "boat_from_environment_99";
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .env("BOATPARTY_BOAT_API_KEY", env_key)
        .args(["--json", "limits"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(env_key));
    assert_eq!(
        boat.requests()[0].authorization.as_deref(),
        Some(format!("Bearer {env_key}").as_str())
    );
}

#[test]
fn a_rate_limited_answer_is_a_typed_failure_with_exit_125() {
    let home = TempDir::new("limits-429");
    store_key(home.path());
    let body = fixture("errors/429-rate-limited-hour.json");
    let boat = FakeBoat::start(move |_| Reply::json(429, &body).header("Retry-After", "1650"));
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .args(["--json", "limits"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    assert_eq!(document["ok"], false);
    assert_eq!(document["error"]["code"], "start_rate_limited");
    let message = document["error"]["message"].as_str().unwrap();
    assert!(message.contains("hour"), "{message}");
    assert!(message.contains("1650"), "{message}");
}

#[test]
fn limits_without_a_key_explains_how_to_get_one() {
    let home = TempDir::new("no-key");
    let output = boatparty(home.path())
        .args(["--json", "limits"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let document = parse_json(&output.stdout);
    assert_eq!(document["error"]["code"], "credentials");
    let message = document["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("boatparty login --create-key"),
        "{message}"
    );
}

#[test]
fn an_unreachable_provider_is_a_connection_failure() {
    let home = TempDir::new("unreachable");
    store_key(home.path());
    // Bind and drop to get a port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let output = boatparty(home.path())
        .env(
            "BOATPARTY_BOAT_API_URL",
            format!("http://127.0.0.1:{port}/api/v1"),
        )
        .args(["--json", "limits"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "connection");
}

#[test]
fn plain_http_to_a_remote_host_is_refused() {
    let home = TempDir::new("plain-http");
    store_key(home.path());
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", "http://boat.example/api/v1")
        .args(["--json", "limits"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(parse_json(&output.stdout)["error"]["code"], "config");
}

#[test]
fn doctor_passes_with_tools_and_a_complete_key() {
    let home = TempDir::new("doctor-ok");
    store_key(home.path());
    let tools = fake_tools(home.path());
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .env("PATH", &tools)
        // No control sockets: the machine's own masters are not this test's.
        .env("BOATPARTY_CONTROL_DIR", "/nonexistent/boatparty-sockets")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    assert_eq!(document["ok"], true);
    let statuses: Vec<(&str, &str)> = document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            (
                check["name"].as_str().unwrap(),
                check["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("ssh", "ok"),
            ("ssh-keygen", "ok"),
            ("git", "ok"),
            ("account", "ok"),
            ("credentials", "ok"),
            ("key", "ok"),
            ("key_scope", "ok"),
            ("pools", "ok"),
            ("ssh_configs", "ok"),
            (
                "control_sockets",
                if cfg!(windows) { "skipped" } else { "ok" }
            ),
        ]
    );
    // One validity call plus one probe per other action; none touch a real sandbox.
    let requests = boat.requests();
    assert_eq!(requests.len(), 16);
    for request in &requests {
        assert!(
            !request.path.starts_with("/sandboxes/")
                || request.path.starts_with("/sandboxes/bx_00000000"),
            "{}",
            request.path
        );
    }
}

#[test]
fn doctor_names_missing_actions_and_the_fix() {
    let home = TempDir::new("doctor-scope");
    store_key(home.path());
    let tools = fake_tools(home.path());
    let limits = fixture("limits.json");
    let not_found = fixture("errors/404-not-found.json");
    let invalid_type = fixture("errors/400-invalid-type.json");
    let forbidden =
        |action: &str| fixture("errors/403-api-key-action-forbidden.json").replace("exec", action);
    let (read, write) = (forbidden("file.read"), forbidden("file.write"));
    let boat = FakeBoat::start(move |request| match request.path.as_str() {
        "/limits" => Reply::json(200, &limits),
        "/sandboxes" => Reply::json(400, &invalid_type),
        path if path.ends_with("/files/download-url") => Reply::json(403, &read),
        path if path.ends_with("/files/upload-url") => Reply::json(403, &write),
        _ => Reply::json(404, &not_found),
    });
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .env("PATH", &tools)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_no_key(&output);
    let document = parse_json(&output.stdout);
    assert_eq!(document["ok"], false);
    assert_eq!(document["error"]["code"], "doctor_failed");
    let scope = document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "key_scope")
        .unwrap();
    assert_eq!(scope["status"], "fail");
    assert!(
        scope["detail"]
            .as_str()
            .unwrap()
            .contains("file.read, file.write")
    );
    assert_eq!(scope["fix"], "boatparty login --create-key");
}

#[cfg(unix)]
#[test]
fn login_create_key_takes_the_key_from_the_boat_cli_through_a_pipe() {
    let home = TempDir::new("login-create");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let argv = home.path().join("boat-argv.log");
    let fake = bin.join("boat");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\necho 'creating a key' >&2\n\
             printf '{{\"ok\":true,\"secret\":\"{TEST_KEY}\",\"id\":\"key_1\"}}\\n'\n",
            argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(["--json", "login", "--create-key"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_key(&output);
    assert_eq!(parse_json(&output.stdout)["verified"], true);
    let stored =
        std::fs::read_to_string(home.path().join("config").join("credentials.toml")).unwrap();
    assert!(stored.contains(TEST_KEY));
    let args = std::fs::read_to_string(&argv).unwrap();
    assert!(
        args.starts_with("api-key create boatparty --ttl 90d --actions sandbox.create,"),
        "{args}"
    );
    assert!(args.trim_end().ends_with("file.write --json"), "{args}");
    assert!(!args.contains(TEST_KEY));
}

#[test]
fn doctor_reports_missing_tools_and_key_with_fixes() {
    let home = TempDir::new("doctor-empty");
    let empty = home.path().join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let output = boatparty(home.path())
        .env("PATH", &empty)
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL  ssh "), "{stdout}");
    assert!(stdout.contains("FAIL  git "), "{stdout}");
    assert!(stdout.contains("FAIL  credentials"), "{stdout}");
    assert!(
        stdout.contains("fix: boatparty login --create-key"),
        "{stdout}"
    );
    assert!(stdout.contains("skip  key "), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("boatparty: 4 checks failed"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn doctor_flags_a_credentials_file_readable_by_others() {
    let home = TempDir::new("doctor-mode");
    store_key(home.path());
    let tools = fake_tools(home.path());
    let path = home.path().join("config").join("credentials.toml");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .env("PATH", &tools)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let document = parse_json(&output.stdout);
    let credentials = &document["checks"][4];
    assert_eq!(credentials["name"], "credentials");
    assert_eq!(credentials["status"], "fail");
    assert_eq!(credentials["fix"], format!("chmod 600 {}", path.display()));
}

#[cfg(windows)]
#[test]
fn doctor_flags_a_credentials_file_readable_by_everyone() {
    let home = TempDir::new("doctor-acl");
    store_key(home.path());
    let path = home.path().join("config").join("credentials.toml");
    let result = std::process::Command::new("icacls.exe")
        .arg(&path)
        .args(["/grant", "*S-1-1-0:R"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let boat = healthy_boat();
    let output = boatparty(home.path())
        .env("BOATPARTY_BOAT_API_URL", boat.url())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let checks = parse_json(&output.stdout);
    let check = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "credentials")
        .unwrap();
    assert_eq!(check["status"], "fail");
    assert_no_key(&output);
}
