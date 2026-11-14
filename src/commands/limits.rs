//! `boatparty limits`: the provider's account limits and usage.

use std::fmt::Write as _;

use serde_json::{Value, json};

use super::open_provider;
use crate::Context;
use crate::error::AppError;
use crate::provider::{AccountLimits, PROVIDER_NAME, WindowUsage};

pub fn run(ctx: &Context) -> Result<u8, AppError> {
    let limits = open_provider(ctx)?.limits()?;
    if ctx.out.is_json() {
        ctx.out.document(&document(&limits));
    } else {
        ctx.out.text(render(&limits).trim_end());
    }
    Ok(0)
}

fn window_json(window: Option<WindowUsage>) -> Value {
    match window {
        Some(window) => json!({
            "limit": window.limit,
            "used": window.used,
            "remaining": window.remaining,
        }),
        None => Value::Null,
    }
}

pub fn document(limits: &AccountLimits) -> Value {
    json!({
        "ok": true,
        "provider": PROVIDER_NAME,
        "can_start": limits.can_start,
        "start_blocked_reason": limits.start_blocked_reason,
        "starts": {
            "unlimited": limits.starts.unlimited,
            "minute": window_json(limits.starts.minute),
            "hour": window_json(limits.starts.hour),
            "day": window_json(limits.starts.day),
        },
        "sandboxes": {
            "running": limits.running_sandboxes,
            "max_running": limits.max_running_sandboxes,
        },
        "included_vm_time": limits.included.as_ref().map(|included| json!({
            "hours": included.vm_hours,
            "hours_remaining": included.vm_hours_remaining,
            "period_end": included.period_end,
        })),
        "sizes": limits.sizes.iter().map(|size| json!({
            "size": size.size.as_str(),
            "available": size.available,
            "reason": size.reason,
        })).collect::<Vec<_>>(),
        "overage": {
            "vm_hour_dollars": limits.overage.vm_hour_dollars,
            "saved_snapshot_day_dollars": limits.overage.saved_snapshot_day_dollars,
        },
        "next_start_dollars": limits.next_start_dollars,
    })
}

pub fn render(limits: &AccountLimits) -> String {
    let mut text = String::new();
    let starts = &limits.starts;
    if starts.unlimited && starts.minute.is_none() && starts.hour.is_none() && starts.day.is_none()
    {
        let _ = writeln!(text, "starts       unlimited");
    } else {
        let _ = writeln!(
            text,
            "starts       {:>6} {:>6} {:>6}",
            "used", "left", "limit"
        );
        for (name, window) in [
            ("minute", starts.minute),
            ("hour", starts.hour),
            ("day", starts.day),
        ] {
            match window {
                Some(window) => {
                    let _ = writeln!(
                        text,
                        "  {name:<10} {:>6} {:>6} {:>6}",
                        window.used, window.remaining, window.limit
                    );
                }
                None => {
                    let _ = writeln!(text, "  {name:<10} unlimited");
                }
            }
        }
    }
    let _ = writeln!(
        text,
        "sandboxes    {} running of at most {}",
        limits.running_sandboxes, limits.max_running_sandboxes
    );
    match &limits.included {
        Some(included) => {
            let until = included
                .period_end
                .as_deref()
                .map(|end| format!(", until {}", end.get(..10).unwrap_or(end)))
                .unwrap_or_default();
            let _ = writeln!(
                text,
                "included VM  {:.1} of {:.1} hours left{until} (hours of a default machine)",
                included.vm_hours_remaining, included.vm_hours
            );
        }
        None => {
            let _ = writeln!(text, "included VM  none reported");
        }
    }
    let available: Vec<&str> = limits
        .sizes
        .iter()
        .filter(|size| size.available)
        .map(|size| size.size.as_str())
        .collect();
    let blocked: Vec<String> = limits
        .sizes
        .iter()
        .filter(|size| !size.available)
        .map(|size| match &size.reason {
            Some(reason) => format!("{} ({reason})", size.size),
            None => size.size.to_string(),
        })
        .collect();
    let _ = write!(
        text,
        "sizes        {}",
        if available.is_empty() {
            "none available".to_string()
        } else {
            available.join(", ")
        }
    );
    if !blocked.is_empty() {
        let _ = write!(text, "; not available: {}", blocked.join(", "));
    }
    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "can start    {}",
        match (&limits.can_start, &limits.start_blocked_reason) {
            (true, _) => "yes".to_string(),
            (false, Some(reason)) => format!("no ({reason})"),
            (false, None) => "no".to_string(),
        }
    );
    text
}
