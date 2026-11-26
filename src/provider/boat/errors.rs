//! Map boat's error answers to typed provider errors.
//!
//! The HTTP status is the primary signal and the envelope's `code` refines
//! it. Fields such as `window` or `retryAfterSeconds` may sit at the top
//! level of the body or inside `error.details`; both are read.

use std::time::Duration;

use serde_json::{Map, Value};

use crate::provider::error::{
    BillingReason, BusyReason, CapacityReason, ConcurrencyReason, ErrorKind, ProviderError,
    ScopeDenial, StartRefusal, StartWindow,
};

/// Build the typed error for a non-success answer.
pub fn map_error(status: u16, retry_after_header: Option<&str>, body: &[u8]) -> ProviderError {
    let parsed: Option<Map<String, Value>> = serde_json::from_slice(body).ok();
    let fields = Fields::new(parsed.as_ref());

    let code = fields.code();
    let message = fields.message();
    let header_delay = retry_after_header.and_then(parse_seconds);
    let body_delay = fields
        .number("retryAfterSeconds")
        .or_else(|| fields.number("retryAfter"))
        .map(duration_from_seconds);
    let retry_after = body_delay.or(header_delay);

    let kind = match code.as_deref() {
        Some(code) => kind_for_code(status, code, &fields, retry_after)
            .unwrap_or_else(|| kind_for_status(status)),
        None => kind_for_status(status),
    };

    let mut error = ProviderError::new(kind)
        .with_status(status)
        .with_request_id(fields.string("requestId"));
    if let Some(code) = code {
        error = error.with_code(code);
    }
    if let Some(message) = message {
        error = error.with_message(message);
    }
    error
}

/// Kind for a recognized code, or `None` for a code this client does not know.
fn kind_for_code(
    status: u16,
    code: &str,
    fields: &Fields<'_>,
    retry_after: Option<Duration>,
) -> Option<ErrorKind> {
    let busy = |reason| ErrorKind::Busy {
        reason,
        retry_after,
    };
    let kind = match code {
        "rate_limited" => ErrorKind::RateLimited(StartRefusal {
            window: fields
                .string("window")
                .and_then(|window| match window.as_str() {
                    "minute" => Some(StartWindow::Minute),
                    "hour" => Some(StartWindow::Hour),
                    "day" => Some(StartWindow::Day),
                    _ => None,
                }),
            limit: fields.number("limit").map(|limit| limit as u64),
            cause: fields.string("cause"),
            next_start_at: fields.string("nextStartAt"),
            retry_after,
        }),
        "IP_RATE_LIMIT_EXCEEDED" => ErrorKind::RequestRateLimited { retry_after },
        "limit_reached" => ErrorKind::ConcurrencyLimit(ConcurrencyReason::Plan),
        "member_limit_reached" => ErrorKind::ConcurrencyLimit(ConcurrencyReason::Member),
        "trial_compute_limit_reached" => ErrorKind::Billing {
            reason: BillingReason::TrialComputeUsed,
            url: None,
        },
        "billing_required" | "credit_required" => ErrorKind::Billing {
            reason: BillingReason::PaymentRequired,
            url: fields.string("billingUrl"),
        },
        "team_member_cap_reached" => ErrorKind::Billing {
            reason: BillingReason::MemberUsageCap,
            url: None,
        },
        "no_ready_machine" => ErrorKind::AtCapacity {
            reason: CapacityReason::NoReadyMachine,
            retry_after,
        },
        "out_of_capacity" => ErrorKind::AtCapacity {
            reason: CapacityReason::OutOfCapacity,
            retry_after,
        },
        "not_found" | "snapshot_not_found" => ErrorKind::NotFound,
        "snapshot_data_expired" => ErrorKind::DataExpired,
        "sandbox_restoring" => busy(BusyReason::Restoring),
        "sandbox_securing" => busy(BusyReason::Securing),
        "ssh_host_key_unavailable" => busy(BusyReason::HostKeyUnavailable),
        "sandbox_starting" | "browser_not_ready" | "desktop_not_ready" => {
            busy(BusyReason::Starting)
        }
        "sandbox_resuming" => busy(BusyReason::Resuming),
        "sandbox_stopping" => busy(BusyReason::Stopping),
        "idempotency_in_progress" => busy(BusyReason::IdempotencyInProgress),
        "save_in_progress" => busy(BusyReason::SaveInProgress),
        "sandbox_direct_failed" => {
            let restoring = fields.message().as_deref() == Some("sandbox_restoring")
                || fields.detail_string("message").as_deref() == Some("sandbox_restoring");
            if restoring {
                busy(BusyReason::Restoring)
            } else if fields.detail_bool("retryable") == Some(true) {
                busy(BusyReason::MachineRetryable)
            } else {
                ErrorKind::Server
            }
        }
        "machine_not_running" | "sandbox_not_ready" => ErrorKind::NotRunning,
        "unauthorized" => ErrorKind::Unauthorized,
        "api_key_action_forbidden" => {
            ErrorKind::ScopeForbidden(ScopeDenial::Action(fields.string("action")))
        }
        "api_key_sandbox_forbidden" => ErrorKind::ScopeForbidden(ScopeDenial::Resource),
        "api_key_expired" => ErrorKind::ScopeForbidden(ScopeDenial::Expired),
        "api_key_org_mismatch" | "api_key_org_forbidden" => {
            ErrorKind::ScopeForbidden(ScopeDenial::Organization)
        }
        "forbidden"
        | "environment_forbidden"
        | "trial_machine_class_not_allowed"
        | "machine_class_plan_required"
        | "org_suspended"
        | "trial_saved_snapshots_not_allowed" => ErrorKind::Forbidden,
        "file_too_large" => ErrorKind::TooLarge,
        "account_not_ready"
        | "provider_not_configured"
        | "sandbox_not_promptable"
        | "resume_failed"
        | "fork_failed"
        | "snapshots_disabled"
        | "idempotency_key_reused"
        | "sandbox_running_stop_first" => ErrorKind::Conflict,
        "invalid_json"
        | "invalid_query"
        | "invalid_name"
        | "invalid_type"
        | "invalid_path"
        | "invalid_timeout"
        | "invalid_env"
        | "unknown_environment"
        | "no_changes"
        | "type_too_small"
        | "trial_auto_stop_required" => ErrorKind::InvalidRequest,
        _ if status >= 500 && (code == "http_500" || code.ends_with("_failed")) => {
            ErrorKind::Server
        }
        _ => return None,
    };
    Some(kind)
}

/// Kind from the status alone: used when the body has no code, or when the
/// code is unknown and the status already says what to do.
fn kind_for_status(status: u16) -> ErrorKind {
    match status {
        400 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Unauthorized,
        404 => ErrorKind::NotFound,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Unknown,
    }
}

/// Read access to an error body: top-level fields, `error`, `error.details`.
struct Fields<'a> {
    top: Option<&'a Map<String, Value>>,
}

impl<'a> Fields<'a> {
    fn new(top: Option<&'a Map<String, Value>>) -> Self {
        Self { top }
    }

    fn error(&self) -> Option<&'a Map<String, Value>> {
        self.top?.get("error")?.as_object()
    }

    fn details(&self) -> Option<&'a Map<String, Value>> {
        self.error()?.get("details")?.as_object()
    }

    /// A field from the top level, else from `error.details`.
    fn get(&self, name: &str) -> Option<&'a Value> {
        self.top
            .and_then(|top| top.get(name))
            .filter(|value| !value.is_null())
            .or_else(|| self.details()?.get(name).filter(|value| !value.is_null()))
    }

    fn string(&self, name: &str) -> Option<String> {
        self.get(name)?.as_str().map(str::to_owned)
    }

    fn number(&self, name: &str) -> Option<f64> {
        let value = self.get(name)?;
        value
            .as_f64()
            .or_else(|| value.as_str()?.trim().parse().ok())
    }

    fn detail_string(&self, name: &str) -> Option<String> {
        self.details()?.get(name)?.as_str().map(str::to_owned)
    }

    fn detail_bool(&self, name: &str) -> Option<bool> {
        self.details()?.get(name)?.as_bool()
    }

    fn code(&self) -> Option<String> {
        self.top
            .and_then(|top| top.get("code"))
            .and_then(Value::as_str)
            .or_else(|| self.error()?.get("code")?.as_str())
            .or_else(|| self.details()?.get("error")?.as_str())
            .filter(|code| !code.is_empty())
            .map(str::to_owned)
    }

    fn message(&self) -> Option<String> {
        self.top
            .and_then(|top| top.get("message"))
            .and_then(Value::as_str)
            .or_else(|| self.error()?.get("message")?.as_str())
            .filter(|message| !message.is_empty())
            .map(str::to_owned)
    }
}

fn parse_seconds(value: &str) -> Option<Duration> {
    value.trim().parse::<f64>().ok().map(duration_from_seconds)
}

fn duration_from_seconds(seconds: f64) -> Duration {
    if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds.min(86_400.0 * 7.0))
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            "rate_limited_hour" => {
                include_bytes!("../../../tests/fixtures/boat/errors/429-rate-limited-hour.json")
            }
            "rate_limited_top_level" => {
                include_bytes!(
                    "../../../tests/fixtures/boat/errors/429-rate-limited-top-level.json"
                )
            }
            "ip_rate_limited" => {
                include_bytes!("../../../tests/fixtures/boat/errors/429-ip-rate-limit.json")
            }
            "limit_reached" => {
                include_bytes!("../../../tests/fixtures/boat/errors/429-limit-reached.json")
            }
            "action_forbidden" => include_bytes!(
                "../../../tests/fixtures/boat/errors/403-api-key-action-forbidden.json"
            ),
            "out_of_capacity" => {
                include_bytes!("../../../tests/fixtures/boat/errors/503-out-of-capacity.json")
            }
            "data_expired" => {
                include_bytes!("../../../tests/fixtures/boat/errors/409-snapshot-data-expired.json")
            }
            "unknown_code" => {
                include_bytes!("../../../tests/fixtures/boat/errors/409-unknown-code.json")
            }
            "not_found" => include_bytes!("../../../tests/fixtures/boat/errors/404-not-found.json"),
            "unauthorized" => {
                include_bytes!("../../../tests/fixtures/boat/errors/401-unauthorized.json")
            }
            "invalid_query" => {
                include_bytes!("../../../tests/fixtures/boat/errors/400-invalid-query.json")
            }
            "restoring" => include_bytes!(
                "../../../tests/fixtures/boat/errors/502-sandbox-direct-failed-restoring.json"
            ),
            "billing" => {
                include_bytes!("../../../tests/fixtures/boat/errors/402-billing-required.json")
            }
            other => panic!("no fixture {other}"),
        }
    }

    #[test]
    fn rate_limited_reads_window_and_retry_from_details() {
        let error = map_error(429, Some("1800"), fixture("rate_limited_hour"));
        let ErrorKind::RateLimited(refusal) = error.kind() else {
            panic!("expected RateLimited, got {:?}", error.kind());
        };
        assert_eq!(refusal.window, Some(StartWindow::Hour));
        assert_eq!(refusal.limit, Some(60));
        assert_eq!(refusal.cause.as_deref(), Some("plan"));
        assert_eq!(
            refusal.next_start_at.as_deref(),
            Some("2026-11-15T01:12:30.000Z")
        );
        // The body's value wins over the header.
        assert_eq!(refusal.retry_after, Some(Duration::from_secs(1650)));
        assert!(error.kind().is_retryable());
        assert_eq!(error.kind().code(), "start_rate_limited");
        assert_eq!(error.status(), Some(429));
        assert_eq!(error.provider_code(), Some("rate_limited"));
        assert!(error.request_id().is_some());
    }

    #[test]
    fn rate_limited_reads_top_level_fields() {
        let error = map_error(429, None, fixture("rate_limited_top_level"));
        let ErrorKind::RateLimited(refusal) = error.kind() else {
            panic!("expected RateLimited, got {:?}", error.kind());
        };
        assert_eq!(refusal.window, Some(StartWindow::Minute));
        assert_eq!(refusal.limit, Some(12));
        assert_eq!(refusal.retry_after, Some(Duration::from_secs(41)));
    }

    #[test]
    fn rate_limited_falls_back_to_the_retry_after_header() {
        let body = br#"{"ok":false,"status":429,"code":"rate_limited","message":"m","error":{"code":"rate_limited","message":"m","status":429,"details":{"window":"day"}}}"#;
        let error = map_error(429, Some("90"), body);
        let ErrorKind::RateLimited(refusal) = error.kind() else {
            panic!("expected RateLimited");
        };
        assert_eq!(refusal.window, Some(StartWindow::Day));
        assert_eq!(refusal.retry_after, Some(Duration::from_secs(90)));
    }

    #[test]
    fn ip_rate_limit_is_its_own_kind() {
        let error = map_error(429, Some("12"), fixture("ip_rate_limited"));
        assert_eq!(
            error.kind(),
            &ErrorKind::RequestRateLimited {
                retry_after: Some(Duration::from_secs(12))
            }
        );
    }

    #[test]
    fn limit_reached_is_the_concurrency_limit() {
        let error = map_error(429, Some("60"), fixture("limit_reached"));
        assert_eq!(
            error.kind(),
            &ErrorKind::ConcurrencyLimit(ConcurrencyReason::Plan)
        );
        assert!(!error.kind().is_retryable());
    }

    #[test]
    fn recorded_action_forbidden_names_the_action() {
        let error = map_error(403, None, fixture("action_forbidden"));
        assert_eq!(
            error.kind(),
            &ErrorKind::ScopeForbidden(ScopeDenial::Action(Some("exec".into())))
        );
        assert_eq!(error.kind().code(), "key_scope_forbidden");
        assert_eq!(
            error.provider_message(),
            Some("This API key cannot perform exec.")
        );
        assert_eq!(
            error.request_id(),
            Some("req_03314311c8eb49eb829ff8e70210b255")
        );
    }

    #[test]
    fn out_of_capacity_is_retryable_with_the_body_delay() {
        let error = map_error(503, None, fixture("out_of_capacity"));
        assert_eq!(
            error.kind(),
            &ErrorKind::AtCapacity {
                reason: CapacityReason::OutOfCapacity,
                retry_after: Some(Duration::from_secs(120)),
            }
        );
        assert!(error.kind().is_retryable());
    }

    #[test]
    fn no_ready_machine_is_at_capacity() {
        let body = br#"{"ok":false,"status":503,"code":"no_ready_machine","message":"No ready machine.","retryAfterSeconds":5}"#;
        let error = map_error(503, None, body);
        assert_eq!(
            error.kind(),
            &ErrorKind::AtCapacity {
                reason: CapacityReason::NoReadyMachine,
                retry_after: Some(Duration::from_secs(5)),
            }
        );
    }

    #[test]
    fn snapshot_data_expired_is_data_expired() {
        let error = map_error(409, None, fixture("data_expired"));
        assert_eq!(error.kind(), &ErrorKind::DataExpired);
        assert!(!error.kind().is_retryable());
    }

    #[test]
    fn unknown_code_keeps_the_raw_code() {
        let error = map_error(409, None, fixture("unknown_code"));
        assert_eq!(error.kind(), &ErrorKind::Unknown);
        assert_eq!(error.provider_code(), Some("brand_new_conflict"));
        assert!(error.to_string().contains("brand_new_conflict"));
    }

    #[test]
    fn unknown_code_on_a_status_with_fixed_meaning_uses_the_status() {
        let body = br#"{"ok":false,"status":404,"code":"gone_somewhere","message":"x"}"#;
        assert_eq!(map_error(404, None, body).kind(), &ErrorKind::NotFound);
        let body = br#"{"ok":false,"status":500,"code":"novel","message":"x"}"#;
        assert_eq!(map_error(500, None, body).kind(), &ErrorKind::Server);
    }

    #[test]
    fn recorded_not_found_unauthorized_and_invalid_query() {
        assert_eq!(
            map_error(404, None, fixture("not_found")).kind(),
            &ErrorKind::NotFound
        );
        assert_eq!(
            map_error(401, None, fixture("unauthorized")).kind(),
            &ErrorKind::Unauthorized
        );
        assert_eq!(
            map_error(400, None, fixture("invalid_query")).kind(),
            &ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn restoring_direct_failure_is_busy() {
        let error = map_error(502, None, fixture("restoring"));
        assert_eq!(
            error.kind(),
            &ErrorKind::Busy {
                reason: BusyReason::Restoring,
                retry_after: None
            }
        );
    }

    #[test]
    fn non_retryable_direct_failure_is_a_server_error() {
        let body = br#"{"ok":false,"status":502,"code":"sandbox_direct_failed","message":"boom","error":{"code":"sandbox_direct_failed","status":502,"details":{"retryable":false,"message":"boom"}}}"#;
        assert_eq!(map_error(502, None, body).kind(), &ErrorKind::Server);
    }

    #[test]
    fn billing_carries_the_billing_url() {
        let error = map_error(402, None, fixture("billing"));
        assert_eq!(
            error.kind(),
            &ErrorKind::Billing {
                reason: BillingReason::PaymentRequired,
                url: Some("https://boat.dev/dashboard?tab=billing".into()),
            }
        );
    }

    #[test]
    fn busy_states_and_scope_codes() {
        let cases = [
            ("sandbox_securing", 409, BusyReason::Securing),
            (
                "ssh_host_key_unavailable",
                503,
                BusyReason::HostKeyUnavailable,
            ),
            ("sandbox_starting", 409, BusyReason::Starting),
            ("sandbox_stopping", 409, BusyReason::Stopping),
            (
                "idempotency_in_progress",
                409,
                BusyReason::IdempotencyInProgress,
            ),
        ];
        for (code, status, reason) in cases {
            let body = format!(r#"{{"ok":false,"status":{status},"code":"{code}","message":"m"}}"#);
            assert_eq!(
                map_error(status, Some("3"), body.as_bytes()).kind(),
                &ErrorKind::Busy {
                    reason,
                    retry_after: Some(Duration::from_secs(3))
                },
                "{code}"
            );
        }
        let scope = [
            ("api_key_sandbox_forbidden", ScopeDenial::Resource),
            ("api_key_expired", ScopeDenial::Expired),
            ("api_key_org_mismatch", ScopeDenial::Organization),
        ];
        for (code, denial) in scope {
            let body = format!(r#"{{"ok":false,"status":403,"code":"{code}","message":"m"}}"#);
            assert_eq!(
                map_error(403, None, body.as_bytes()).kind(),
                &ErrorKind::ScopeForbidden(denial),
                "{code}"
            );
        }
    }

    #[test]
    fn non_json_bodies_map_by_status() {
        assert_eq!(
            map_error(502, None, b"<html>bad gateway</html>").kind(),
            &ErrorKind::Server
        );
        assert_eq!(map_error(401, None, b"").kind(), &ErrorKind::Unauthorized);
        let error = map_error(418, None, b"teapot");
        assert_eq!(error.kind(), &ErrorKind::Unknown);
        assert_eq!(error.status(), Some(418));
    }

    #[test]
    fn messages_never_repeat_a_bare_code() {
        let error = map_error(404, None, fixture("not_found"));
        assert_eq!(
            error.to_string(),
            "not found (not_found) [request req_460f397fb1cc460eb1564bee9d3e5768]"
        );
    }
}
