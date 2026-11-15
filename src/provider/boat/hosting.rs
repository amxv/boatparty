//! Public URLs for sandbox ports (`POST /sandboxes/{id}/host`).
//!
//! A gated URL carries its access token (`_token`), so the answer is read
//! by hand: a decoding error never quotes the body, and nothing here logs.

use serde_json::{Value, json};

use super::http::{Http, Method};
use super::sandboxes::checked_id;
use crate::provider::{ErrorKind, HostPort, HostedPort, ProviderError};

pub fn host_port(http: &Http, id: &str, request: &HostPort) -> Result<HostedPort, ProviderError> {
    let id = checked_id(id)?;
    let mut body = json!({
        "port": request.port,
        "public": request.public,
        "localhost": request.localhost,
    });
    if let Some(title) = &request.title {
        body["title"] = json!(title);
    }
    let answer = http.send(
        Method::Post,
        &format!("/sandboxes/{id}/host"),
        Some(&body),
        None,
    )?;
    if !answer.is_success() {
        return Err(answer.error());
    }
    let status = answer.status;
    let value: Value = serde_json::from_slice(&answer.body)
        .map_err(|_| invalid(status, "the answer to exposing a port is not JSON"))?;
    parse(&value, request.port)
        .ok_or_else(|| invalid(status, "the answer to exposing a port carries no https URL"))
}

/// The hosted port from the answer, or `None` when it has no https URL.
fn parse(value: &Value, port: u16) -> Option<HostedPort> {
    let url = value.get("url").and_then(Value::as_str)?;
    if !url.starts_with("https://") {
        return None;
    }
    let protected = match value.get("isProtected").and_then(Value::as_bool) {
        Some(protected) => protected,
        None => value.get("access").and_then(Value::as_str) != Some("public"),
    };
    Some(HostedPort {
        port: value
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(port),
        url: url.to_string(),
        protected,
        localhost: value
            .get("localhost")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn invalid(status: u16, message: &str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse)
        .with_status(status)
        .with_message(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_gives_the_url_and_whether_it_is_gated() {
        let gated = parse(
            &json!({"ok": true, "type": "port.hosted", "success": true, "port": 3000,
                    "url": "https://otter-3000.on.boat.dev?_token=abc", "isProtected": true,
                    "access": "private"}),
            3000,
        )
        .unwrap();
        assert!(gated.protected);
        assert_eq!(gated.url, "https://otter-3000.on.boat.dev?_token=abc");
        assert!(
            !format!("{gated:?}").contains("abc"),
            "Debug shows the token"
        );

        let open = parse(
            &json!({"url": "https://otter-3000.on.boat.dev", "access": "public",
                    "localhost": true}),
            3000,
        )
        .unwrap();
        assert!(!open.protected);
        assert!(open.localhost);
        assert_eq!(open.port, 3000);

        assert!(parse(&json!({"url": "http://plain"}), 1).is_none());
        assert!(parse(&json!({"ok": true}), 1).is_none());
    }
}
