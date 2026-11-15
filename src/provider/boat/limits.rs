//! `GET /limits`: the subset of boat's `LimitsFields` boatparty reads.

use serde::Deserialize;

use crate::provider::{
    AccountLimits, IncludedTime, MachineSize, OveragePrices, SizeAvailability, StartUsage,
    StartWindow, WindowFull, WindowUsage,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsBody {
    can_start: bool,
    #[serde(default)]
    start_blocked_reason: Option<String>,
    active_sandboxes: u64,
    max_active_sandboxes: u64,
    #[serde(default)]
    starts: Option<StartsBody>,
    #[serde(default)]
    start_window_full: Option<WindowFullBody>,
    #[serde(default)]
    plan_included: Option<PlanIncludedBody>,
    #[serde(default)]
    machine_types: Vec<MachineTypeBody>,
    #[serde(default)]
    overage_prices: Option<OveragePricesBody>,
    #[serde(default)]
    next_start_dollars: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct StartsBody {
    #[serde(default)]
    unlimited: bool,
    #[serde(default)]
    minute: Option<WindowBody>,
    #[serde(default)]
    hour: Option<WindowBody>,
    #[serde(default)]
    day: Option<WindowBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowFullBody {
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    frees_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WindowBody {
    limit: u64,
    used: u64,
    remaining: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanIncludedBody {
    vm_hours: f64,
    vm_hours_remaining: f64,
    #[serde(default)]
    period_end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MachineTypeBody {
    #[serde(rename = "type")]
    size: String,
    available: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OveragePricesBody {
    #[serde(default)]
    vm_hour_dollars: Option<f64>,
    #[serde(default)]
    saved_snapshot_day_dollars: Option<f64>,
}

impl From<WindowBody> for WindowUsage {
    fn from(window: WindowBody) -> Self {
        Self {
            limit: window.limit,
            used: window.used,
            remaining: window.remaining,
        }
    }
}

impl From<LimitsBody> for AccountLimits {
    fn from(body: LimitsBody) -> Self {
        let starts = match body.starts {
            Some(starts) => StartUsage {
                unlimited: starts.unlimited,
                minute: starts.minute.map(Into::into),
                hour: starts.hour.map(Into::into),
                day: starts.day.map(Into::into),
            },
            None => StartUsage {
                unlimited: true,
                minute: None,
                hour: None,
                day: None,
            },
        };
        // A size this client does not know is skipped rather than guessed.
        let sizes = body
            .machine_types
            .into_iter()
            .filter_map(|machine| {
                Some(SizeAvailability {
                    size: machine.size.parse::<MachineSize>().ok()?,
                    available: machine.available,
                    reason: machine.reason,
                })
            })
            .collect();
        let overage = body
            .overage_prices
            .map(|prices| OveragePrices {
                vm_hour_dollars: prices.vm_hour_dollars,
                saved_snapshot_day_dollars: prices.saved_snapshot_day_dollars,
            })
            .unwrap_or_default();
        let window_full = body.start_window_full.and_then(|full| {
            let window = match full.window.as_deref()? {
                "minute" => StartWindow::Minute,
                "hour" => StartWindow::Hour,
                "day" => StartWindow::Day,
                _ => return None,
            };
            Some(WindowFull {
                window,
                frees_at: full.frees_at,
            })
        });
        Self {
            can_start: body.can_start,
            start_blocked_reason: body.start_blocked_reason,
            starts,
            window_full,
            running_sandboxes: body.active_sandboxes,
            max_running_sandboxes: body.max_active_sandboxes,
            included: body.plan_included.map(|plan| IncludedTime {
                vm_hours: plan.vm_hours,
                vm_hours_remaining: plan.vm_hours_remaining,
                period_end: plan.period_end,
            }),
            sizes,
            overage,
            next_start_dollars: body.next_start_dollars,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_recorded_limits_answer() {
        let body: LimitsBody =
            serde_json::from_str(include_str!("../../../tests/fixtures/boat/limits.json")).unwrap();
        let limits = AccountLimits::from(body);
        assert!(limits.can_start);
        assert!(!limits.starts.unlimited);
        assert_eq!(
            limits.starts.minute,
            Some(WindowUsage {
                limit: 12,
                used: 0,
                remaining: 12
            })
        );
        assert_eq!(
            limits.starts.hour,
            Some(WindowUsage {
                limit: 60,
                used: 8,
                remaining: 52
            })
        );
        assert_eq!(
            limits.starts.day,
            Some(WindowUsage {
                limit: 200,
                used: 50,
                remaining: 150
            })
        );
        assert_eq!(limits.running_sandboxes, 2);
        assert_eq!(limits.max_running_sandboxes, 100);
        let included = limits.included.unwrap();
        assert_eq!(included.vm_hours, 555.56);
        assert_eq!(included.vm_hours_remaining, 450.5);
        assert_eq!(
            included.period_end.as_deref(),
            Some("2026-12-01T00:00:00.000Z")
        );
        assert_eq!(limits.sizes.len(), 4);
        assert_eq!(limits.sizes[3].size, MachineSize::XLarge);
        assert!(!limits.sizes[3].available);
        assert_eq!(limits.overage.vm_hour_dollars, Some(0.036));
        assert_eq!(limits.next_start_dollars, Some(0.0));
        assert_eq!(limits.window_full, None);
    }

    #[test]
    fn a_full_window_says_when_it_frees() {
        let body: LimitsBody = serde_json::from_str(
            r#"{"canStart":false,"activeSandboxes":0,"maxActiveSandboxes":5,
                "starts":{"unlimited":false,"minute":{"limit":12,"used":12,"remaining":0}},
                "startWindowFull":{"window":"minute","limit":12,"freesAt":"2026-11-15T10:00:30Z","paidBy":null}}"#,
        )
        .unwrap();
        let limits = AccountLimits::from(body);
        assert_eq!(
            limits.window_full,
            Some(WindowFull {
                window: StartWindow::Minute,
                frees_at: Some("2026-11-15T10:00:30Z".into())
            })
        );
    }

    #[test]
    fn unlimited_accounts_have_no_windows() {
        let body: LimitsBody = serde_json::from_str(
            r#"{"canStart":true,"activeSandboxes":0,"maxActiveSandboxes":5,"billingStatus":"active",
                "starts":{"unlimited":true,"minute":null,"hour":null,"day":null},
                "planIncluded":null,"machineTypes":[{"type":"huge","available":true,"reason":null}]}"#,
        )
        .unwrap();
        let limits = AccountLimits::from(body);
        assert!(limits.starts.unlimited);
        assert_eq!(limits.starts.day, None);
        assert_eq!(limits.included, None);
        assert!(limits.sizes.is_empty());
    }
}
