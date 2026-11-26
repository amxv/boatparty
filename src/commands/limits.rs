//! `boatparty limits`: the provider's account limits and usage, then each
//! project's usage against its own limits; `limits set` changes them.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Value, json};

use super::open_provider;
use crate::Context;
use crate::account::ProjectSettings;
use crate::account::edit::{self, Target};
use crate::cli::{LimitsAction, LimitsArgs, LimitsTarget};
use crate::error::AppError;
use crate::pool::LABEL_PROJECT;
use crate::provider::{AccountLimits, PROVIDER_NAME, Provider, SandboxState, WindowUsage};
use crate::starts::{self, cache::LimitsCache};
use crate::state::{Role, State, Store};
use crate::system;

pub fn run(ctx: &Context, args: &LimitsArgs) -> Result<u8, AppError> {
    match &args.action {
        Some(LimitsAction::Set(target)) => set(ctx, target),
        Some(LimitsAction::Starts) => super::ledger::run(ctx),
        None => show(ctx),
    }
}

/// One project's usage against its limits.
struct ProjectUsage {
    name: String,
    settings: ProjectSettings,
    members: usize,
    /// Sandboxes with a machine, from the provider's listing.
    running: Option<usize>,
    starts_hour: usize,
    starts_day: usize,
    /// Machine time this month: (dollars, seconds, since).
    cost: Option<(f64, u64, Option<String>)>,
    error: Option<String>,
}

fn show(ctx: &Context) -> Result<u8, AppError> {
    let account = super::load_account(ctx)?;
    let provider = open_provider(ctx)?;
    let requested_at = system::now_ms();
    let limits = provider.limits()?;
    let store = Store::new(&ctx.paths);
    // The figures just read serve the start ledger too.
    let _ = store.update(|state| {
        LimitsCache::from_limits(&limits, requested_at).write(state);
        Ok(())
    });
    let state = store.load()?;
    let now = system::now_ms();
    let mut names: BTreeSet<String> = account.config.projects.keys().cloned().collect();
    names.extend(state.projects.keys().cloned());
    let projects: Vec<ProjectUsage> = names
        .into_iter()
        .map(|name| usage(provider.as_ref(), &state, &account.config, name, now))
        .collect();
    let (hour, day) = ledger_totals(&state, now);
    let settings = &account.config.account;
    if ctx.out.is_json() {
        let mut document = document(&limits);
        document["account"] = json!({
            "file": account.path.display().to_string(),
            "max_running": settings.max_running,
            "reserve_starts_per_hour": settings.reserve_starts_per_hour,
            "reserve_starts_per_day": settings.reserve_starts_per_day,
            "paid_starts": settings.paid_starts.unwrap_or(true),
            "boatparty_starts": {"hour": hour, "day": day},
        });
        document["projects"] = Value::Array(projects.iter().map(project_json).collect());
        ctx.out.document(&document);
    } else {
        let mut text = render(&limits);
        let _ = writeln!(
            text,
            "account      max_running {}; reserve {} starts/hour, {} starts/day; paid starts {}; \
             boatparty started {hour} in the last hour, {day} in the last day",
            settings
                .max_running
                .map_or_else(|| "unset".to_string(), |max| max.to_string()),
            settings.reserve_starts_per_hour,
            settings.reserve_starts_per_day,
            if settings.paid_starts.unwrap_or(true) {
                "allowed"
            } else {
                "off"
            },
        );
        if !projects.is_empty() {
            let _ = writeln!(text);
            text.push_str(&render_projects(&projects));
        }
        ctx.out.text(text.trim_end());
    }
    Ok(0)
}

fn usage(
    provider: &dyn Provider,
    state: &State,
    config: &crate::account::AccountConfig,
    name: String,
    now: u64,
) -> ProjectUsage {
    let settings = ProjectSettings::resolve(config, &name);
    let members = state.project(&name).map_or(0, |pool| {
        pool.members
            .iter()
            .filter(|m| m.role != Role::Builder)
            .count()
    });
    let (starts_hour, starts_day) = starts::project_usage(state, &name, now);
    let mut usage = ProjectUsage {
        name,
        settings,
        members,
        running: None,
        starts_hour,
        starts_day,
        cost: None,
        error: None,
    };
    match provider.list_by_label_with_cost(LABEL_PROJECT, &usage.name) {
        Ok(listed) => {
            usage.running = Some(
                listed
                    .iter()
                    .filter(|(sandbox, _)| {
                        matches!(
                            sandbox.state,
                            SandboxState::Running | SandboxState::Starting
                        )
                    })
                    .count(),
            );
            let costs: Vec<_> = listed
                .iter()
                .filter_map(|(_, cost)| cost.as_ref())
                .collect();
            if !costs.is_empty() {
                usage.cost = Some((
                    costs.iter().map(|cost| cost.dollars).sum(),
                    costs.iter().map(|cost| cost.seconds).sum(),
                    costs.iter().find_map(|cost| cost.since.clone()),
                ));
            } else if listed.is_empty() {
                usage.cost = Some((0.0, 0, None));
            }
        }
        Err(error) => usage.error = Some(error.to_string()),
    }
    usage
}

/// boatparty's own starts of every project: (last hour, last day).
fn ledger_totals(state: &State, now: u64) -> (usize, usize) {
    let count = |span: u64| {
        state
            .starts
            .iter()
            .filter(|record| record.at + span > now)
            .count()
    };
    (
        count(starts::admission::HOUR_MS),
        count(starts::admission::DAY_MS),
    )
}

fn project_json(usage: &ProjectUsage) -> Value {
    let settings = &usage.settings;
    json!({
        "project": usage.name,
        "members": usage.members,
        "running": usage.running,
        "max_sandboxes": settings.max_sandboxes,
        "max_running": settings.max_running,
        "machine": settings.machine.as_str(),
        "max_machine": settings.max_machine.map(|size| size.as_str()),
        "lease_minutes": settings.lease_minutes,
        "idle_minutes": settings.idle_minutes,
        "starts": {
            "hour": {"used": usage.starts_hour, "cap": settings.starts_per_hour},
            "day": {"used": usage.starts_day, "cap": settings.starts_per_day},
        },
        "cost": usage.cost.as_ref().map(|(dollars, seconds, since)| json!({
            "dollars": (dollars * 100.0).round() / 100.0,
            "seconds": seconds,
            "since": since,
        })),
        "error": usage.error,
    })
}

fn render_projects(projects: &[ProjectUsage]) -> String {
    let of = |used: usize, cap: Option<u32>| match cap {
        Some(cap) => format!("{used}/{cap}"),
        None => used.to_string(),
    };
    let mut rows = vec![[
        "PROJECT".to_string(),
        "MEMBERS".to_string(),
        "RUNNING".to_string(),
        "STARTS HOUR".to_string(),
        "STARTS DAY".to_string(),
        "MACHINE".to_string(),
        "IDLE/LEASE".to_string(),
        "COST THIS MONTH".to_string(),
    ]];
    for usage in projects {
        let settings = &usage.settings;
        let running = match usage.running {
            Some(running) => of(running, settings.max_running),
            None => "?".to_string(),
        };
        let machine = match settings.max_machine {
            Some(max) if max != settings.machine => format!("{} (max {max})", settings.machine),
            _ => settings.machine.to_string(),
        };
        let cost = match &usage.cost {
            Some((dollars, seconds, _)) => {
                format!("${dollars:.2} ({:.1} h)", *seconds as f64 / 3600.0)
            }
            None => "?".to_string(),
        };
        rows.push([
            usage.name.clone(),
            of(usage.members, Some(settings.max_sandboxes)),
            running,
            of(usage.starts_hour, settings.starts_per_hour),
            of(usage.starts_day, settings.starts_per_day),
            machine,
            format!("{}m/{}m", settings.idle_minutes, settings.lease_minutes),
            cost,
        ]);
    }
    let widths: Vec<usize> = (0..8)
        .map(|column| rows.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let mut text = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        let _ = writeln!(text, "{}", line.join("  ").trim_end());
    }
    for usage in projects {
        if let Some(error) = &usage.error {
            let _ = writeln!(text, "{}: cannot list its sandboxes: {error}", usage.name);
        }
    }
    text
}

fn set(ctx: &Context, target: &LimitsTarget) -> Result<u8, AppError> {
    let (target, assignments) = match target {
        LimitsTarget::Account { assignments } => (Target::Account, assignments),
        LimitsTarget::Project { name, assignments } => (Target::Project(name), assignments),
    };
    let (changes, path) = edit::set(&ctx.paths, target, assignments)?;
    if ctx.out.is_json() {
        ctx.out.document(&json!({
            "ok": true,
            "file": path.display().to_string(),
            "scope": match target { Target::Account => "account", Target::Project(_) => "project" },
            "project": match target { Target::Account => None, Target::Project(name) => Some(name) },
            "changes": changes.iter().map(|change| json!({
                "table": change.table,
                "key": change.key,
                "old": change.old,
                "new": change.new,
            })).collect::<Vec<_>>(),
        }));
    } else {
        let shown = |value: &Option<Value>| match value {
            Some(value) => value.to_string(),
            None => "unset".to_string(),
        };
        for change in &changes {
            ctx.out.text(format_args!(
                "[{}] {}: {} -> {}",
                change.table,
                change.key,
                shown(&change.old),
                shown(&change.new)
            ));
        }
        ctx.out.text(format_args!("saved {}", path.display()));
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
        "extra_starts": limits.extra_starts,
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
            (true, _)
                if limits.extra_starts && limits.next_start_dollars.is_some_and(|d| d > 0.0) =>
                format!(
                    "yes, as a paid extra start (${:.2} on boat's next invoice)",
                    limits.next_start_dollars.unwrap_or_default()
                ),
            (true, _) => "yes".to_string(),
            (false, Some(reason)) => format!("no ({reason})"),
            (false, None) => "no".to_string(),
        }
    );
    text
}
