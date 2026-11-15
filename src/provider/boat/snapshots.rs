//! boat's saved snapshots: save, inspect, list (with the plan's allowance)
//! and remove.

use serde::Deserialize;
use serde_json::{Value, json};

use super::http::{Http, Method};
use super::sandboxes::checked_id;
use crate::provider::{ErrorKind, ProviderError, SavedSnapshot, SnapshotAllowance, SnapshotStatus};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotBody {
    name: String,
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    source_sandbox_id: Option<String>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    snapshot_id: Option<String>,
}

impl SnapshotBody {
    fn into_snapshot(self) -> Result<SavedSnapshot, ProviderError> {
        Ok(SavedSnapshot {
            status: map_status(&self.status)?,
            name: self.name,
            error: self.error,
            source_id: self.source_sandbox_id,
            size_bytes: self.size_bytes,
            created_at: self.created_at,
            artifact: self.snapshot_id,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OneBody {
    snapshot: SnapshotBody,
}

#[derive(Debug, Deserialize)]
struct ListBody {
    #[serde(default)]
    snapshots: Vec<SnapshotBody>,
    #[serde(default)]
    allowance: Option<AllowanceBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AllowanceBody {
    #[serde(default)]
    included: Option<u64>,
    #[serde(default)]
    used: Option<u64>,
    #[serde(default)]
    extra: Option<u64>,
    #[serde(default)]
    extra_monthly_dollars: Option<f64>,
    #[serde(default)]
    price_per_extra_daily_dollars: Option<f64>,
}

/// The documented statuses; anything else is an error, never a guess.
pub fn map_status(status: &str) -> Result<SnapshotStatus, ProviderError> {
    Ok(match status {
        "saving" => SnapshotStatus::Saving,
        "ready" => SnapshotStatus::Ready,
        "failed" => SnapshotStatus::Failed,
        other => {
            return Err(ProviderError::new(ErrorKind::InvalidResponse)
                .with_message(format!("unknown saved snapshot status {other:?}")));
        }
    })
}

/// Saved-snapshot names are boat's pattern `^[a-z0-9][a-z0-9-]{0,62}$`;
/// refuse anything else before it reaches a URL.
fn checked_name(name: &str) -> Result<&str, ProviderError> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(name)
    } else {
        Err(ProviderError::new(ErrorKind::InvalidRequest)
            .with_message(format!("not a saved snapshot name: {name:?}")))
    }
}

pub fn save(http: &Http, sandbox_id: &str, name: &str) -> Result<SavedSnapshot, ProviderError> {
    let sandbox_id = checked_id(sandbox_id)?;
    let name = checked_name(name)?;
    let body: OneBody = http
        .send(
            Method::Post,
            "/saved-snapshots",
            Some(&json!({ "sandboxId": sandbox_id, "name": name })),
            None,
        )?
        .json()?;
    body.snapshot.into_snapshot()
}

pub fn get(http: &Http, name: &str) -> Result<Option<SavedSnapshot>, ProviderError> {
    let name = checked_name(name)?;
    let answer = http.send(Method::Get, &format!("/saved-snapshots/{name}"), None, None)?;
    if answer.status == 404 {
        return Ok(None);
    }
    let body: OneBody = answer.json()?;
    body.snapshot.into_snapshot().map(Some)
}

pub fn list(
    http: &Http,
    name: &str,
) -> Result<(Vec<SavedSnapshot>, Option<SnapshotAllowance>), ProviderError> {
    let name = checked_name(name)?;
    let body: ListBody = http
        .send(
            Method::Get,
            &format!("/saved-snapshots?name={name}"),
            None,
            None,
        )?
        .json()?;
    let snapshots = body
        .snapshots
        .into_iter()
        .map(SnapshotBody::into_snapshot)
        .collect::<Result<Vec<_>, _>>()?;
    let allowance = body.allowance.map(|allowance| SnapshotAllowance {
        included: allowance.included,
        used: allowance.used,
        extra: allowance.extra,
        extra_monthly_dollars: allowance.extra_monthly_dollars,
        price_per_extra_daily_dollars: allowance.price_per_extra_daily_dollars,
    });
    Ok((snapshots, allowance))
}

/// Remove a saved snapshot; `false` when it did not exist.
pub fn delete(http: &Http, name: &str) -> Result<bool, ProviderError> {
    let name = checked_name(name)?;
    let answer = http.send(
        Method::Delete,
        &format!("/saved-snapshots/{name}"),
        None,
        None,
    )?;
    if answer.status == 404 {
        return Ok(false);
    }
    let _: Value = answer.json()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_and_unknown_ones_are_errors() {
        assert_eq!(map_status("saving").unwrap(), SnapshotStatus::Saving);
        assert_eq!(map_status("ready").unwrap(), SnapshotStatus::Ready);
        assert_eq!(map_status("failed").unwrap(), SnapshotStatus::Failed);
        assert!(map_status("archived").is_err());
    }

    #[test]
    fn a_snapshot_body_is_read() {
        let body: OneBody = serde_json::from_value(json!({
            "snapshot": {"name": "bp-app", "status": "ready", "sourceSandboxId": "bx_1",
                         "sizeBytes": 1234, "createdAt": "2026-11-15T00:00:00Z", "type": "default"}
        }))
        .unwrap();
        let snapshot = body.snapshot.into_snapshot().unwrap();
        assert_eq!(snapshot.name, "bp-app");
        assert_eq!(snapshot.status, SnapshotStatus::Ready);
        assert_eq!(snapshot.size_bytes, Some(1234));
        assert_eq!(snapshot.source_id.as_deref(), Some("bx_1"));
    }

    #[test]
    fn names_that_could_change_the_url_are_refused() {
        assert!(checked_name("bp-app").is_ok());
        for bad in ["", "Bp-app", "-x", "a/b", "a?b", &"a".repeat(64)] {
            assert!(checked_name(bad).is_err(), "{bad}");
        }
    }
}
