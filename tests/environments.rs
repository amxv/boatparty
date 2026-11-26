//! Environment API and command boundaries, including hostile provider errors.
#![cfg(unix)]
mod common;
use common::{
    FakeBoat, Reply,
    e2e::{E2e, ID},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn unconfigured_project_accepts_the_default_pin_but_flags_a_named_pin() {
    let setup = E2e::new("default-environment");
    let boat = FakeBoat::start(|r| {
        let value = match (r.method.as_str(), r.path.split('?').next().unwrap()) {
            ("GET", "/environments") => json!({"environments":[]}),
            ("GET", "/sandboxes") => json!({"sandboxes":[
                {"id":ID,"state":"idle","environment":"base","environmentVersion":1},
                {"id":"bx_other","state":"idle","environment":"other","environmentVersion":1}
            ]}),
            _ => return Reply::json(404, "{}"),
        };
        Reply::json(200, &value.to_string())
    });
    for args in [vec!["env", "status"], vec!["env", "apply", "--all"]] {
        let output = setup
            .cmd(&boat)
            .arg("--json")
            .args(&args)
            .assert()
            .success();
        let doc: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
        let members = &doc["members"];
        assert_eq!(doc["environment"], Value::Null);
        assert_eq!(members[0]["environment"], "base");
        assert_eq!(members[0]["version"], 1);
        assert_eq!(members[0]["warning"], Value::Null);
        assert_eq!(members[1]["warning"], "env: other v1 (expected none)");
        if args.contains(&"apply") {
            assert_eq!(members[0]["action"], "current");
            assert_eq!(members[1]["action"], "recreate");
        }
    }
}

#[test]
fn safe_creation_secret_changes_and_scoped_member_upgrade() {
    let setup = E2e::new("environment");
    let version = AtomicU64::new(1);
    let pinned = AtomicU64::new(1);
    const MARKER: &str = "fixture-value-4ba0";
    let boat = FakeBoat::start(move |r| {
        let path = r.path.split('?').next().unwrap();
        let v = version.load(Ordering::SeqCst);
        let entry = json!({"id":"e-1","name":"app","latestVersionNumber":v,
            "envContents":format!("TOKEN={MARKER}"),"passSecrets":true,"safeForThirdParties":false});
        let sandbox = json!({"id":ID,"state":"idle","name":"bp-app-1","environment":"app","environmentVersion":pinned.load(Ordering::SeqCst)});
        let value = match (r.method.as_str(), path) {
            ("GET" | "POST", "/environments") => json!({"environments":[entry]}),
            ("PUT", "/environments/e-1") => {
                let body: Value = serde_json::from_str(&r.body).unwrap();
                for k in [
                    "passGithub",
                    "passAgentsCredentials",
                    "passSandboxCredentials",
                    "proxyAiCredentials",
                ] {
                    assert_eq!(body[k], false);
                }
                assert_eq!(body["passSecrets"], true);
                json!({"environment":entry})
            }
            ("PUT" | "DELETE", "/environments/e-1/vars/TOKEN") => {
                if r.method == "PUT" {
                    assert_eq!(
                        serde_json::from_str::<Value>(&r.body).unwrap()["value"],
                        MARKER
                    );
                }
                json!({"success":true,"versionNumber":version.fetch_add(1,Ordering::SeqCst)+1})
            }
            ("PUT", "/environments/e-1/vars/FAIL") => {
                return Reply::json(400, &json!({"message":MARKER}).to_string());
            }
            ("POST", "/environments/e-1/upgrade") => {
                assert_eq!(
                    serde_json::from_str::<Value>(&r.body).unwrap(),
                    json!({"agentIds":[ID]})
                );
                pinned.store(v, Ordering::SeqCst);
                json!({"success":true,"failed":0})
            }
            ("GET", "/sandboxes") => {
                json!({"sandboxes":[sandbox,{"id":"bx_other","name":"bp-app-2","state":"idle","environment":"other","environmentVersion":2}]})
            }
            ("GET", _) if path.ends_with(ID) => json!({"sandbox":sandbox}),
            _ => return Reply::json(404, "{}"),
        };
        Reply::json(200, &value.to_string())
    });
    let call = |args: &[&str], input: Option<&str>, shape: &str| {
        let mut cmd = setup.cmd(&boat);
        cmd.arg("--json").args(args);
        if let Some(input) = input {
            cmd.write_stdin(input);
        }
        let out = cmd.output().unwrap();
        assert!(!String::from_utf8_lossy(&out.stderr).contains(MARKER));
        assert!(!String::from_utf8_lossy(&out.stdout).contains(MARKER));
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        common::contract::assert_shape(shape, &doc);
        assert_eq!(out.status.success(), doc["ok"] == true, "{doc}");
        doc
    };
    call(
        &["secret", "set", "--create", "TOKEN"],
        Some(MARKER),
        "secret set",
    );
    assert_eq!(
        call(&["secret", "list"], None, "secret list")["names"],
        json!(["TOKEN"])
    );
    let state = call(&["env", "status"], None, "env status");
    assert!(
        state["members"][0]["warning"]
            .as_str()
            .unwrap()
            .contains("expected app v2")
    );
    assert_eq!(state["members"][0]["names"], json!(["TOKEN"]));
    let applied = call(&["env", "apply", "--all"], None, "env apply");
    assert_eq!(applied["members"][0]["action"], "upgraded");
    assert_eq!(applied["members"][0]["names"], json!(["TOKEN"]));
    assert_eq!(applied["members"][1]["action"], "recreate");
    call(&["secret", "rm", "TOKEN"], None, "secret rm");
    call(&["secret", "set", "FAIL"], Some(MARKER), "error");
    call(&["secret", "set", "TOKEN", MARKER], None, "error");
    assert!(
        !std::fs::read_to_string(setup.home().join("state/state.json"))
            .unwrap()
            .contains(MARKER)
    );
    assert!(!setup.home().join("state/runs.jsonl").exists());
}
