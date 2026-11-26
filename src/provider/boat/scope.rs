//! Which boat API key actions boatparty needs, and a side-effect-free check
//! that a key carries them.
//!
//! A scoped key cannot read its own scope (`GET /api-keys` is empty for
//! restricted keys), so each action is probed. boat checks the key's action
//! before it looks up the target, so a request against a sandbox that cannot
//! exist answers `403 api_key_action_forbidden` when the action is missing
//! and `404 not_found` when it is present. `bx_00000000` cannot exist: `0` is
//! outside boat's sandbox id alphabet. The create probe sends an invalid
//! machine type, which boat refuses with `400` after the scope check.

use std::thread;

use serde_json::{Value, json};

use super::http::{Answer, Http, Method};
use crate::provider::{CredentialCheck, CredentialHelp, ErrorKind, ProviderError, ScopeDenial};

/// Every action boatparty uses (measured against boat; see docs/boat-facts.md).
pub const REQUIRED_ACTIONS: [&str; 14] = [
    "sandbox.create",
    "sandbox.read",
    "sandbox.update",
    "sandbox.stop",
    "sandbox.resume",
    "sandbox.delete",
    "ssh",
    "host",
    "desktop",
    "snapshot.read",
    "snapshot.write",
    "account.read",
    "file.read",
    "file.write",
];

const PROBE_SANDBOX: &str = "bx_00000000";
const PROBE_SNAPSHOT: &str = "boatparty-scope-probe";

pub fn help() -> CredentialHelp {
    let actions = REQUIRED_ACTIONS.join(",");
    let create_command = format!("boat api-key create boatparty --ttl 90d --actions {actions}");
    CredentialHelp {
        permissions: REQUIRED_ACTIONS.to_vec(),
        create_and_login_command: "boatparty login --create-key".to_string(),
        create_command,
        create_program: "boat",
        create_args: [
            "api-key",
            "create",
            "boatparty",
            "--ttl",
            "90d",
            "--actions",
            &actions,
            "--json",
        ]
        .map(String::from)
        .to_vec(),
    }
}

struct Probe {
    action: &'static str,
    method: Method,
    path: String,
    body: Option<Value>,
    header: Option<(&'static str, String)>,
}

fn probes() -> Vec<Probe> {
    let sandbox = |action, method, suffix: &str, body: Option<Value>| Probe {
        action,
        method,
        path: format!("/sandboxes/{PROBE_SANDBOX}{suffix}"),
        body,
        header: None,
    };
    vec![
        Probe {
            action: "sandbox.create",
            method: Method::Post,
            path: "/sandboxes".into(),
            body: Some(json!({ "type": "boatparty-scope-probe", "ttlSeconds": 60, "noEnv": true })),
            header: None,
        },
        sandbox("sandbox.read", Method::Get, "", None),
        sandbox(
            "sandbox.update",
            Method::Patch,
            "",
            Some(json!({ "ttlSeconds": 60 })),
        ),
        sandbox("sandbox.stop", Method::Post, "/stop", Some(json!({}))),
        sandbox("sandbox.resume", Method::Post, "/resume", Some(json!({}))),
        Probe {
            header: Some(("X-Ascii-Confirm-Delete", PROBE_SANDBOX.to_string())),
            ..sandbox("sandbox.delete", Method::Delete, "", None)
        },
        sandbox(
            "ssh",
            Method::Post,
            "/sshkey",
            Some(json!({ "key": "ssh-ed25519 AAAA boatparty-scope-probe" })),
        ),
        sandbox("desktop", Method::Post, "/desktop", Some(json!({}))),
        sandbox("host", Method::Post, "/host", Some(json!({ "port": 8000 }))),
        Probe {
            action: "snapshot.read",
            method: Method::Get,
            path: format!("/saved-snapshots/{PROBE_SNAPSHOT}"),
            body: None,
            header: None,
        },
        Probe {
            action: "snapshot.write",
            method: Method::Post,
            path: "/saved-snapshots".into(),
            body: Some(json!({ "sandboxId": PROBE_SANDBOX, "name": PROBE_SNAPSHOT })),
            header: None,
        },
        sandbox(
            "file.read",
            Method::Post,
            "/files/download-url",
            Some(json!({ "path": "boatparty-scope-probe" })),
        ),
        sandbox(
            "file.write",
            Method::Post,
            "/files/upload-url",
            Some(json!({ "path": "boatparty-scope-probe" })),
        ),
    ]
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Allowed,
    Missing(String),
    Restricted,
    Expired,
    Invalid,
}

/// Classify one probe answer.
fn classify(action: &str, answer: &Answer) -> Result<Outcome, ProviderError> {
    if answer.is_success() {
        return Ok(Outcome::Allowed);
    }
    let error = answer.error();
    match error.kind() {
        ErrorKind::Unauthorized => Ok(Outcome::Invalid),
        ErrorKind::ScopeForbidden(ScopeDenial::Expired) => Ok(Outcome::Expired),
        ErrorKind::ScopeForbidden(ScopeDenial::Action(named)) => Ok(Outcome::Missing(
            named.clone().unwrap_or_else(|| action.to_string()),
        )),
        ErrorKind::ScopeForbidden(ScopeDenial::Resource | ScopeDenial::Organization) => {
            Ok(Outcome::Restricted)
        }
        // Past the scope check: the action is on the key.
        ErrorKind::NotFound
        | ErrorKind::InvalidRequest
        | ErrorKind::Conflict
        | ErrorKind::NotRunning
        | ErrorKind::Forbidden => Ok(Outcome::Allowed),
        _ => Err(error),
    }
}

pub fn check(http: &Http) -> Result<CredentialCheck, ProviderError> {
    // `account.read` doubles as the validity check.
    let first = classify(
        "account.read",
        &http.send(Method::Get, "/limits", None, None)?,
    )?;
    match first {
        Outcome::Invalid => return Ok(CredentialCheck::Invalid),
        Outcome::Expired => return Ok(CredentialCheck::Expired),
        _ => {}
    }

    let results: Vec<Result<Outcome, ProviderError>> = thread::scope(|scope| {
        let handles: Vec<_> = probes()
            .into_iter()
            .map(|probe| {
                let http = http.clone();
                scope.spawn(move || run_probe(&http, &probe))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    Err(ProviderError::new(ErrorKind::InvalidResponse)
                        .with_message("a scope probe panicked"))
                })
            })
            .collect()
    });

    let mut outcomes = vec![first];
    for result in results {
        outcomes.push(result?);
    }
    Ok(summarize(outcomes))
}

fn run_probe(http: &Http, probe: &Probe) -> Result<Outcome, ProviderError> {
    let header = probe
        .header
        .as_ref()
        .map(|(name, value)| (*name, value.as_str()));
    let answer = http.send(probe.method, &probe.path, probe.body.as_ref(), header)?;
    if probe.action == "sandbox.create" && answer.is_success() {
        return Err(unexpected_create(http, &answer));
    }
    classify(probe.action, &answer)
}

/// The create probe can never succeed with an invalid machine type. If it
/// ever did, delete what it made and say so.
fn unexpected_create(http: &Http, answer: &Answer) -> ProviderError {
    let id = serde_json::from_slice::<Value>(&answer.body)
        .ok()
        .and_then(|body| body["sandbox"]["id"].as_str().map(str::to_owned));
    let cleanup = match &id {
        Some(id) => match http.send(
            Method::Delete,
            &format!("/sandboxes/{id}"),
            None,
            Some(("X-Ascii-Confirm-Delete", id)),
        ) {
            Ok(answer) if answer.is_success() => format!("deleted sandbox {id}"),
            _ => format!("could not delete sandbox {id}; delete it by hand"),
        },
        None => "the answer named no sandbox".to_string(),
    };
    ProviderError::new(ErrorKind::InvalidResponse).with_message(format!(
        "the scope probe for sandbox.create was accepted instead of refused ({cleanup})"
    ))
}

fn summarize(outcomes: Vec<Outcome>) -> CredentialCheck {
    if outcomes.contains(&Outcome::Invalid) {
        return CredentialCheck::Invalid;
    }
    if outcomes.contains(&Outcome::Expired) {
        return CredentialCheck::Expired;
    }
    let mut missing: Vec<String> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Missing(action) => Some(action.clone()),
            _ => None,
        })
        .collect();
    if !missing.is_empty() {
        missing.sort_by_key(|action| {
            REQUIRED_ACTIONS
                .iter()
                .position(|required| required == action)
                .unwrap_or(usize::MAX)
        });
        missing.dedup();
        return CredentialCheck::MissingPermissions(missing);
    }
    if outcomes.contains(&Outcome::Restricted) {
        return CredentialCheck::RestrictedResources;
    }
    CredentialCheck::Complete
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, body: &str) -> Answer {
        Answer {
            status,
            retry_after: None,
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn every_required_action_has_one_probe_except_account_read() {
        let probed: Vec<&str> = probes().iter().map(|probe| probe.action).collect();
        for action in REQUIRED_ACTIONS {
            let count = probed.iter().filter(|probed| **probed == action).count();
            let expected = usize::from(action != "account.read");
            assert_eq!(count, expected, "{action}");
        }
    }

    #[test]
    fn probes_never_target_a_real_sandbox_or_a_valid_machine_type() {
        for probe in probes() {
            if probe.path.starts_with("/sandboxes/") {
                assert!(
                    probe.path.starts_with("/sandboxes/bx_00000000"),
                    "{}",
                    probe.path
                );
            }
        }
        let create = probes()
            .into_iter()
            .find(|probe| probe.action == "sandbox.create")
            .unwrap();
        let size = create.body.unwrap()["type"].as_str().unwrap().to_string();
        assert!(size.parse::<crate::provider::MachineSize>().is_err());
    }

    #[test]
    fn classifies_recorded_answers() {
        let forbidden =
            include_str!("../../../tests/fixtures/boat/errors/403-api-key-action-forbidden.json");
        assert_eq!(
            classify("exec", &answer(403, forbidden)).unwrap(),
            Outcome::Missing("exec".into())
        );
        let not_found = include_str!("../../../tests/fixtures/boat/errors/404-not-found.json");
        assert_eq!(
            classify("ssh", &answer(404, not_found)).unwrap(),
            Outcome::Allowed
        );
        let invalid_type =
            include_str!("../../../tests/fixtures/boat/errors/400-invalid-type.json");
        assert_eq!(
            classify("sandbox.create", &answer(400, invalid_type)).unwrap(),
            Outcome::Allowed
        );
        let unauthorized =
            include_str!("../../../tests/fixtures/boat/errors/401-unauthorized.json");
        assert_eq!(
            classify("account.read", &answer(401, unauthorized)).unwrap(),
            Outcome::Invalid
        );
        assert!(classify("ssh", &answer(503, r#"{"code":"out_of_capacity"}"#)).is_err());
    }

    #[test]
    fn summary_prefers_invalid_then_expired_then_missing() {
        assert_eq!(
            summarize(vec![Outcome::Allowed, Outcome::Missing("host".into())]),
            CredentialCheck::MissingPermissions(vec!["host".into()])
        );
        assert_eq!(
            summarize(vec![
                Outcome::Missing("file.write".into()),
                Outcome::Missing("ssh".into()),
                Outcome::Restricted,
            ]),
            CredentialCheck::MissingPermissions(vec!["ssh".into(), "file.write".into()])
        );
        assert_eq!(
            summarize(vec![Outcome::Restricted, Outcome::Allowed]),
            CredentialCheck::RestrictedResources
        );
        assert_eq!(
            summarize(vec![Outcome::Expired, Outcome::Missing("ssh".into())]),
            CredentialCheck::Expired
        );
        assert_eq!(summarize(vec![Outcome::Allowed]), CredentialCheck::Complete);
    }

    #[test]
    fn help_lists_every_action_once() {
        let help = help();
        assert_eq!(help.permissions.len(), REQUIRED_ACTIONS.len());
        assert!(help.create_command.contains(&REQUIRED_ACTIONS.join(",")));
        assert_eq!(
            help.create_and_login_command,
            "boatparty login --create-key"
        );
        // `login --create-key` runs this, with the same actions and JSON out.
        assert_eq!(help.create_program, "boat");
        assert_eq!(help.create_args[..3], ["api-key", "create", "boatparty"]);
        assert!(help.create_args.contains(&REQUIRED_ACTIONS.join(",")));
        assert_eq!(help.create_args.last().map(String::as_str), Some("--json"));
    }
}
