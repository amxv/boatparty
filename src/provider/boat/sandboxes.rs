//! boat's sandbox lifecycle endpoints and their mapping to the neutral types.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::http::{Http, Method};
use crate::provider::{
    CreateSandbox, Deletion, DeletionStatus, ErrorKind, Hydration, ProviderError, Resumed, Sandbox,
    SandboxState, SshAccess,
};

/// boat's header that must equal the sandbox id for a delete to be accepted.
const CONFIRM_DELETE_HEADER: &str = "X-Ascii-Confirm-Delete";
const PAGE_SIZE: u32 = 200;

/// Map one of boat's documented `state` strings. Anything else is an error:
/// an unrecognized state is never guessed to be stopped.
pub fn map_state(state: &str) -> Result<SandboxState, ProviderError> {
    Ok(match state {
        "init" | "provisioning" | "provisioned" | "cloning" => SandboxState::Starting,
        "ready" | "idle" | "running" => SandboxState::Running,
        "archiving" => SandboxState::Stopping,
        "archived" => SandboxState::Stopped,
        "error" => SandboxState::Error,
        "cancelled" => SandboxState::Gone,
        other => {
            return Err(ProviderError::new(ErrorKind::InvalidResponse)
                .with_message(format!("unknown sandbox state {other:?}")));
        }
    })
}

/// `hydrated: true` is the only end signal; an absent field is `Unknown`.
pub fn map_hydration(hydrated: Option<bool>, progress: Option<&HydrationBody>) -> Hydration {
    match hydrated {
        Some(true) => Hydration::Complete,
        Some(false) => Hydration::InProgress {
            percent: progress
                .and_then(|progress| progress.percent)
                .and_then(|percent| u8::try_from(percent.clamp(0, 100)).ok()),
        },
        None => Hydration::Unknown,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxBody {
    id: String,
    #[serde(default)]
    name: Option<String>,
    state: String,
    #[serde(default)]
    hydrated: Option<bool>,
    #[serde(default)]
    hydration: Option<HydrationBody>,
    #[serde(default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "type")]
    size: Option<String>,
    #[serde(default)]
    archive_after: Option<String>,
    #[serde(default)]
    data_expired_at: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HydrationBody {
    #[serde(default)]
    percent: Option<i64>,
}

impl SandboxBody {
    fn into_sandbox(self) -> Result<Sandbox, ProviderError> {
        Ok(Sandbox {
            state: map_state(&self.state)?,
            hydration: map_hydration(self.hydrated, self.hydration.as_ref()),
            id: self.id,
            name: self.name,
            labels: self.labels.unwrap_or_default(),
            size: self.size.and_then(|size| size.parse().ok()),
            auto_stop_at: self.archive_after,
            data_expired_at: self.data_expired_at,
            error: self.error,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ListBody {
    sandboxes: Vec<SandboxBody>,
    #[serde(default, rename = "pageInfo")]
    page_info: Option<PageInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct OneBody {
    sandbox: SandboxBody,
}

#[derive(Debug, Deserialize)]
struct ActionBody {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    sandbox: Option<SandboxBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SshKeyBody {
    #[serde(default)]
    machine_ip: Option<String>,
    #[serde(default)]
    ssh_user: Option<String>,
    #[serde(default)]
    ssh_endpoint: Option<String>,
    #[serde(default)]
    host_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeletionBody {
    operation: OperationBody,
}

#[derive(Debug, Deserialize)]
struct OperationBody {
    id: String,
    status: String,
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse).with_message(message)
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Sandbox ids are short tokens; refuse anything that could change the URL.
fn checked_id(id: &str) -> Result<&str, ProviderError> {
    if !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        Ok(id)
    } else {
        Err(ProviderError::new(ErrorKind::InvalidRequest)
            .with_message(format!("not a sandbox id: {id:?}")))
    }
}

pub fn list_by_label(http: &Http, key: &str, value: &str) -> Result<Vec<Sandbox>, ProviderError> {
    let label = encode(&format!("{key}={value}"));
    let mut cursor: Option<String> = None;
    let mut out = Vec::new();
    loop {
        let mut path = format!("/sandboxes?label={label}&limit={PAGE_SIZE}");
        if let Some(cursor) = &cursor {
            path.push_str(&format!("&cursor={}", encode(cursor)));
        }
        let body: ListBody = http.send(Method::Get, &path, None, None)?.json()?;
        for sandbox in body.sandboxes {
            out.push(sandbox.into_sandbox()?);
        }
        match body.page_info {
            Some(PageInfo {
                next_cursor: Some(next),
                has_more: true,
            }) => cursor = Some(next),
            _ => return Ok(out),
        }
    }
}

pub fn get(http: &Http, id: &str) -> Result<Sandbox, ProviderError> {
    let id = checked_id(id)?;
    let answer = http.send(Method::Get, &format!("/sandboxes/{id}"), None, None)?;
    if answer.status == 404 {
        return Ok(Sandbox::gone(id));
    }
    let body: OneBody = answer.json()?;
    body.sandbox.into_sandbox()
}

pub fn create_body(request: &CreateSandbox) -> Value {
    json!({
        "name": request.name,
        "type": request.size.as_str(),
        "ttlSeconds": request.ttl_seconds,
        "labels": request.labels,
        "noEnv": request.no_env,
    })
}

pub fn create(http: &Http, request: &CreateSandbox) -> Result<Sandbox, ProviderError> {
    let body = create_body(request);
    let answer = http.send(
        Method::Post,
        "/sandboxes",
        Some(&body),
        Some(("Idempotency-Key", &request.idempotency_key)),
    )?;
    let body: OneBody = answer.json()?;
    body.sandbox.into_sandbox()
}

pub fn resume(http: &Http, id: &str, ttl_seconds: u64) -> Result<Resumed, ProviderError> {
    let id = checked_id(id)?;
    let answer = http.send(
        Method::Post,
        &format!("/sandboxes/{id}/resume"),
        Some(&json!({ "ttlSeconds": ttl_seconds })),
        None,
    )?;
    // A reclaim of the kept machine answers 200 with status `running`; a
    // resume onto a new machine answers 202 with status `resuming`.
    let status = answer.status;
    let body: ActionBody = answer.json()?;
    let reclaimed = status == 200 && body.status.as_deref() == Some("running");
    let sandbox = body.sandbox.map(SandboxBody::into_sandbox).transpose()?;
    Ok(Resumed { reclaimed, sandbox })
}

pub fn stop(http: &Http, id: &str) -> Result<(), ProviderError> {
    let id = checked_id(id)?;
    let _: Value = http
        .send(
            Method::Post,
            &format!("/sandboxes/{id}/stop"),
            Some(&json!({})),
            None,
        )?
        .json()?;
    Ok(())
}

pub fn set_ttl(http: &Http, id: &str, ttl_seconds: u64) -> Result<(), ProviderError> {
    let id = checked_id(id)?;
    let _: Value = http
        .send(
            Method::Patch,
            &format!("/sandboxes/{id}"),
            Some(&json!({ "ttlSeconds": ttl_seconds })),
            None,
        )?
        .json()?;
    Ok(())
}

fn deletion_status(status: &str) -> Result<DeletionStatus, ProviderError> {
    Ok(match status {
        "pending" => DeletionStatus::Pending,
        "processing" => DeletionStatus::Processing,
        "blocked" => DeletionStatus::Blocked,
        "completed" => DeletionStatus::Completed,
        other => return Err(invalid(format!("unknown deletion status {other:?}"))),
    })
}

fn into_deletion(body: DeletionBody) -> Result<Deletion, ProviderError> {
    Ok(Deletion {
        status: deletion_status(&body.operation.status)?,
        operation_id: body.operation.id,
    })
}

pub fn delete(http: &Http, id: &str) -> Result<Deletion, ProviderError> {
    let id = checked_id(id)?;
    let body: DeletionBody = http
        .send(
            Method::Delete,
            &format!("/sandboxes/{id}"),
            None,
            Some((CONFIRM_DELETE_HEADER, id)),
        )?
        .json()?;
    into_deletion(body)
}

pub fn deletion(http: &Http, operation_id: &str) -> Result<Deletion, ProviderError> {
    let operation_id = checked_id(operation_id)?;
    let body: DeletionBody = http
        .send(
            Method::Get,
            &format!("/deletion-operations/{operation_id}"),
            None,
            None,
        )?
        .json()?;
    into_deletion(body)
}

/// Connect to `sshEndpoint` (`host:port`) when set, else to `machineIp` on 22.
pub fn parse_ssh_access(
    machine_ip: Option<&str>,
    endpoint: Option<&str>,
    user: Option<&str>,
    host_key: Option<&str>,
) -> Result<SshAccess, ProviderError> {
    let host_key = host_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| invalid("the SSH answer has no host key"))?;
    if host_key.contains('\n') || host_key.split_whitespace().count() < 2 {
        return Err(invalid("the SSH host key is malformed"));
    }
    let (host, port) = match endpoint.filter(|endpoint| !endpoint.is_empty()) {
        Some(endpoint) => {
            let (host, port) = endpoint
                .rsplit_once(':')
                .ok_or_else(|| invalid(format!("malformed SSH endpoint {endpoint:?}")))?;
            let port = port
                .parse::<u16>()
                .map_err(|_| invalid(format!("malformed SSH endpoint {endpoint:?}")))?;
            (host.trim_matches(['[', ']']).to_string(), port)
        }
        None => (
            machine_ip
                .filter(|ip| !ip.is_empty())
                .ok_or_else(|| invalid("the SSH answer has neither an endpoint nor an address"))?
                .to_string(),
            22,
        ),
    };
    let safe_host = |host: &str| {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-'))
    };
    if !safe_host(&host) {
        return Err(invalid(format!("malformed SSH host {host:?}")));
    }
    let user = user.filter(|user| !user.is_empty()).unwrap_or("user");
    if !user
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(invalid(format!("malformed SSH user {user:?}")));
    }
    Ok(SshAccess {
        host,
        port,
        user: user.to_string(),
        host_key: host_key.to_string(),
    })
}

pub fn authorize_ssh_key(
    http: &Http,
    id: &str,
    public_key: &str,
) -> Result<SshAccess, ProviderError> {
    let id = checked_id(id)?;
    let body: SshKeyBody = http
        .send(
            Method::Post,
            &format!("/sandboxes/{id}/sshkey"),
            Some(&json!({ "key": public_key })),
            None,
        )?
        .json()?;
    parse_ssh_access(
        body.machine_ip.as_deref(),
        body.ssh_endpoint.as_deref(),
        body.ssh_user.as_deref(),
        body.host_key.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MachineSize;

    #[test]
    fn every_documented_state_maps_to_its_variant() {
        let table = [
            ("init", SandboxState::Starting),
            ("provisioning", SandboxState::Starting),
            ("provisioned", SandboxState::Starting),
            ("cloning", SandboxState::Starting),
            ("ready", SandboxState::Running),
            ("idle", SandboxState::Running),
            ("running", SandboxState::Running),
            ("archiving", SandboxState::Stopping),
            ("archived", SandboxState::Stopped),
            ("error", SandboxState::Error),
            ("cancelled", SandboxState::Gone),
        ];
        for (raw, expected) in table {
            assert_eq!(map_state(raw).unwrap(), expected, "{raw}");
        }
    }

    #[test]
    fn unknown_states_are_errors() {
        // The CLI's words and anything new are not guessed.
        for raw in ["stopped", "stopping", "paused", "", "ARCHIVED"] {
            let error = map_state(raw).unwrap_err();
            assert_eq!(error.kind(), &ErrorKind::InvalidResponse, "{raw}");
        }
    }

    fn parse(body: Value) -> Result<Sandbox, ProviderError> {
        serde_json::from_value::<SandboxBody>(body)
            .unwrap()
            .into_sandbox()
    }

    #[test]
    fn missing_hydrated_is_unknown_not_complete() {
        let sandbox = parse(json!({"id": "bx_1", "state": "ready"})).unwrap();
        assert_eq!(sandbox.hydration, Hydration::Unknown);
        assert!(!sandbox.hydration.is_complete());

        let sandbox = parse(json!({
            "id": "bx_1", "state": "ready", "hydrated": false,
            "hydration": {"phase": "hydrating", "entriesDone": 1, "entriesTotal": 4, "percent": 25}
        }))
        .unwrap();
        assert_eq!(
            sandbox.hydration,
            Hydration::InProgress { percent: Some(25) }
        );

        let sandbox = parse(json!({"id": "bx_1", "state": "idle", "hydrated": true})).unwrap();
        assert_eq!(sandbox.hydration, Hydration::Complete);
    }

    #[test]
    fn a_sandbox_body_with_an_unknown_state_is_an_error() {
        assert!(parse(json!({"id": "bx_1", "state": "hibernating"})).is_err());
    }

    #[test]
    fn sandbox_fields_are_read() {
        let sandbox = parse(json!({
            "id": "bx_23456789", "name": "bp-app-1", "state": "archived",
            "labels": {"boatparty.project": "app", "boatparty.role": "pool"},
            "type": "default", "archiveAfter": null,
            "dataExpiredAt": "2026-12-01T00:00:00Z", "hydrated": true
        }))
        .unwrap();
        assert_eq!(sandbox.state, SandboxState::Stopped);
        assert_eq!(sandbox.size, Some(MachineSize::Default));
        assert_eq!(sandbox.labels["boatparty.role"], "pool");
        assert_eq!(
            sandbox.data_expired_at.as_deref(),
            Some("2026-12-01T00:00:00Z")
        );
    }

    #[test]
    fn create_body_carries_no_env_labels_and_size() {
        let request = CreateSandbox {
            name: "bp-app-1".into(),
            size: MachineSize::Default,
            ttl_seconds: 900,
            labels: BTreeMap::from([
                ("boatparty.project".to_string(), "app".to_string()),
                ("boatparty.role".to_string(), "pool".to_string()),
            ]),
            no_env: true,
            idempotency_key: "k".into(),
        };
        let body = create_body(&request);
        assert_eq!(body["noEnv"], true);
        assert_eq!(body["type"], "default");
        assert_eq!(body["ttlSeconds"], 900);
        assert_eq!(body["labels"]["boatparty.project"], "app");
        assert!(body.get("env").is_none());
    }

    #[test]
    fn ssh_access_prefers_the_endpoint() {
        let access = parse_ssh_access(
            Some("2001:db8::1"),
            Some("203.0.113.10:22001"),
            Some("user"),
            Some("ssh-ed25519 AAAA"),
        )
        .unwrap();
        assert_eq!((access.host.as_str(), access.port), ("203.0.113.10", 22001));
        let access =
            parse_ssh_access(Some("198.51.100.7"), None, None, Some("ssh-ed25519 AAAA")).unwrap();
        assert_eq!((access.host.as_str(), access.port), ("198.51.100.7", 22));
        assert_eq!(access.user, "user");
        assert!(parse_ssh_access(Some("1.2.3.4"), None, None, None).is_err());
        assert!(parse_ssh_access(None, None, None, Some("ssh-ed25519 AAAA")).is_err());
        assert!(parse_ssh_access(Some("h;rm -rf"), None, None, Some("ssh-ed25519 AAAA")).is_err());
        assert!(
            parse_ssh_access(Some("1.2.3.4"), None, None, Some("ssh-ed25519 AAAA\nevil")).is_err()
        );
    }

    #[test]
    fn label_queries_are_encoded() {
        assert_eq!(encode("boatparty.project=app"), "boatparty.project%3Dapp");
    }

    #[test]
    fn ids_that_could_change_the_url_are_refused() {
        assert!(checked_id("bx_23456789").is_ok());
        assert!(checked_id("bdop_8c86906803da420b85becdfc03862f30").is_ok());
        assert!(checked_id("../limits").is_err());
        assert!(checked_id("").is_err());
    }
}
