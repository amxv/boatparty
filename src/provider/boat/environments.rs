//! Only names and version metadata leave this module. Provider bodies can hold
//! the contents of every variable and secret file, including on errors.
use super::http::{Http, Method};
use crate::provider::{Environment, ErrorKind, ProviderError};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse)
}

fn request<T: DeserializeOwned>(
    http: &Http,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<T, ProviderError> {
    let answer = http.send(method, path, body, None)?;
    if !answer.is_success() {
        let kind = match answer.status {
            401 => ErrorKind::Unauthorized,
            403 => ErrorKind::Forbidden,
            404 => ErrorKind::NotFound,
            409 => ErrorKind::Conflict,
            400..=499 => ErrorKind::InvalidRequest,
            _ => ErrorKind::Server,
        };
        return Err(ProviderError::new(kind)
            .with_status(answer.status)
            .with_message(
                "environment request refused; check the environment name and key permissions",
            ));
    }
    serde_json::from_slice(&answer.body).map_err(|_| invalid())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    name: String,
    latest_version_number: u64,
    env_contents: Option<String>,
    pass_secrets: bool,
    safe_for_third_parties: bool,
}

fn metadata(entry: Entry) -> Result<Environment, ProviderError> {
    if entry.id.is_empty()
        || !entry
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid());
    }
    Ok(Environment {
        id: entry.id,
        name: entry.name,
        version: entry.latest_version_number,
        injects_secrets: entry.pass_secrets && !entry.safe_for_third_parties,
        names: entry.env_contents.as_deref().and_then(names),
    })
}

// Refuse ambiguous dotenv syntax. In particular, a continuation of a quoted
// value can look like a variable assignment and must never become a name.
fn names(contents: &str) -> Option<Vec<String>> {
    let mut names = std::collections::BTreeSet::new();
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .strip_prefix("export ")
            .unwrap_or(line)
            .split_once('=')?;
        let key = key.trim();
        if !crate::provider::valid_variable(key) {
            return None;
        }
        let value = value.trim();
        if let Some(quote) = value
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"' | '`'))
        {
            let mut escaped = false;
            let mut closed = false;
            for (i, c) in value.char_indices().skip(1) {
                if !escaped && c == quote {
                    let tail = value[i + 1..].trim_start();
                    if !tail.is_empty() && !tail.starts_with('#') {
                        return None;
                    }
                    closed = true;
                    break;
                }
                escaped = !escaped && c == '\\';
            }
            if !closed {
                return None;
            }
        } else if value.ends_with('\\') {
            return None;
        }
        names.insert(key.to_string());
    }
    Some(names.into_iter().collect())
}

#[derive(Deserialize)]
struct List {
    environments: Vec<Entry>,
}

pub(super) fn list(http: &Http) -> Result<Vec<Environment>, ProviderError> {
    request::<List>(http, Method::Get, "/environments", None)?
        .environments
        .into_iter()
        .map(metadata)
        .collect()
}

pub(super) fn create(http: &Http, name: &str) -> Result<Environment, ProviderError> {
    let list: List = request(
        http,
        Method::Post,
        "/environments",
        Some(&json!({"name":name})),
    )?;
    let entry = metadata(
        list.environments
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(invalid)?,
    )?;
    #[derive(Deserialize)]
    struct Updated {
        environment: Entry,
    }
    let updated: Updated = request(
        http,
        Method::Put,
        &format!("/environments/{}", entry.id),
        Some(
            &json!({"passGithub":false,"passSecrets":true,"passSandboxCredentials":false,"passAgentsCredentials":false,"proxyAiCredentials":false}),
        ),
    )?;
    metadata(updated.environment)
}

pub(super) fn change(
    http: &Http,
    id: &str,
    key: &str,
    value: Option<&str>,
) -> Result<u64, ProviderError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Changed {
        success: bool,
        version_number: u64,
    }
    let path = format!("/environments/{id}/vars/{key}");
    let response: Changed = match value {
        Some(value) => request(http, Method::Put, &path, Some(&json!({"value":value})))?,
        None => request(http, Method::Delete, &path, None)?,
    };
    response
        .success
        .then_some(response.version_number)
        .ok_or_else(invalid)
}

pub(super) fn upgrade(http: &Http, id: &str, sandbox: &str) -> Result<(), ProviderError> {
    #[derive(Deserialize)]
    struct Changed {
        success: bool,
        failed: u64,
    }
    let changed: Changed = request(
        http,
        Method::Post,
        &format!("/environments/{id}/upgrade"),
        Some(&json!({"agentIds":[sandbox]})),
    )?;
    if !changed.success || changed.failed > 0 {
        return Err(ProviderError::new(ErrorKind::Conflict)
            .with_message("the provider failed to upgrade the member"));
    }
    Ok(())
}
