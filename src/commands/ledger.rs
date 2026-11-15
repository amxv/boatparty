//! `boatparty limits starts`: boatparty's own start ledger beside the
//! account's figures from the provider, so the rolling windows can be read
//! and planned around.
//!
//! The ledger only knows boatparty's starts (every create and every resume
//! onto a new machine it made, on this machine). The provider counts every
//! start of the account; the difference is starts made elsewhere (CI, other
//! tools, other machines, by hand).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Value, json};

use super::open_provider;
use crate::Context;
use crate::error::AppError;
use crate::provider::{AccountLimits, StartWindow, WindowUsage};
use crate::runs::rfc3339;
use crate::starts::admission::{DAY_MS, HOUR_MS, MINUTE_MS};
use crate::starts::cache::LimitsCache;
use crate::state::{StartRecord, Store};
use crate::system;

/// Hours ahead the free-up forecast covers.
const FORECAST_HOURS: u64 = 24;
/// Starts listed one by one in text mode (all of them in JSON).
const TEXT_TIMELINE: usize = 15;

pub fn run(ctx: &Context) -> Result<u8, AppError> {
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
    let view = LedgerView::build(&state.starts, Some(&limits), now);
    if ctx.out.is_json() {
        ctx.out.document(&view.json());
    } else {
        ctx.out.text(view.text().trim_end());
    }
    Ok(0)
}

/// One rolling window: boatparty's starts in it, the account's figures and
/// the gap between them.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowView {
    pub window: StartWindow,
    pub boatparty: usize,
    pub projects: BTreeMap<String, usize>,
    pub account: Option<WindowUsage>,
    /// Account starts boatparty did not make.
    pub outside: Option<u64>,
    /// When boatparty's oldest start in the window ages out of it.
    pub next_frees_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LedgerView {
    pub now: u64,
    pub windows: Vec<WindowView>,
    /// Starts of the last day, oldest first: (at, project, in flight).
    pub starts: Vec<(u64, String, bool)>,
    /// For each of the coming hours, how many of boatparty's starts leave
    /// the day window during it.
    pub frees_by_hour: Vec<usize>,
    /// When the provider says its full window frees, if one is full.
    pub provider_frees: Option<(StartWindow, String)>,
    /// The account pays for starts past its limits.
    pub extra_starts: bool,
    pub next_start_dollars: Option<f64>,
}

fn span(window: StartWindow) -> u64 {
    match window {
        StartWindow::Minute => MINUTE_MS,
        StartWindow::Hour => HOUR_MS,
        StartWindow::Day => DAY_MS,
    }
}

impl LedgerView {
    /// Pure: the view of `ledger` and the provider's `limits` at `now` (ms).
    pub fn build(ledger: &[StartRecord], limits: Option<&AccountLimits>, now: u64) -> Self {
        let mut day: Vec<&StartRecord> = ledger
            .iter()
            .filter(|record| record.at + DAY_MS > now && record.at <= now)
            .collect();
        day.sort_by_key(|record| record.at);
        let windows = [StartWindow::Minute, StartWindow::Hour, StartWindow::Day]
            .into_iter()
            .map(|window| {
                let inside: Vec<&&StartRecord> = day
                    .iter()
                    .filter(|record| record.at + span(window) > now)
                    .collect();
                let mut projects = BTreeMap::new();
                for record in &inside {
                    *projects.entry(record.project.clone()).or_insert(0) += 1;
                }
                let account = limits.and_then(|limits| match window {
                    StartWindow::Minute => limits.starts.minute,
                    StartWindow::Hour => limits.starts.hour,
                    StartWindow::Day => limits.starts.day,
                });
                WindowView {
                    window,
                    boatparty: inside.len(),
                    projects,
                    outside: account.map(|usage| usage.used.saturating_sub(inside.len() as u64)),
                    account,
                    next_frees_at: inside.first().map(|record| record.at + span(window)),
                }
            })
            .collect();
        let frees_by_hour = (0..FORECAST_HOURS)
            .map(|hour| {
                let from = now + hour * HOUR_MS;
                let until = from + HOUR_MS;
                day.iter()
                    .filter(|record| {
                        let leaves = record.at + DAY_MS;
                        leaves > from && leaves <= until
                    })
                    .count()
            })
            .collect();
        Self {
            now,
            windows,
            starts: day
                .iter()
                .map(|record| (record.at, record.project.clone(), record.done_at.is_none()))
                .collect(),
            frees_by_hour,
            provider_frees: limits.and_then(|limits| {
                let full = limits.window_full.as_ref()?;
                Some((full.window, full.frees_at.clone()?))
            }),
            extra_starts: limits.is_some_and(|limits| limits.extra_starts),
            next_start_dollars: limits.and_then(|limits| limits.next_start_dollars),
        }
    }

    pub fn json(&self) -> Value {
        let at = |ms: Option<u64>| ms.map(rfc3339);
        json!({
            "ok": true,
            "now": rfc3339(self.now),
            "windows": self.windows.iter().map(|view| json!({
                "window": view.window.as_str(),
                "boatparty": view.boatparty,
                "projects": view.projects,
                "account": view.account.map(|usage| json!({
                    "limit": usage.limit,
                    "used": usage.used,
                    "remaining": usage.remaining,
                })),
                "outside_boatparty": view.outside,
                "next_frees_at": at(view.next_frees_at),
            })).collect::<Vec<_>>(),
            "starts": self.starts.iter().map(|(ms, project, in_flight)| json!({
                "at": rfc3339(*ms),
                "project": project,
                "in_flight": in_flight,
            })).collect::<Vec<_>>(),
            "frees_by_hour": self.frees_by_hour.iter().enumerate().map(|(hour, frees)| json!({
                "until": rfc3339(self.now + (hour as u64 + 1) * HOUR_MS),
                "frees": frees,
            })).collect::<Vec<_>>(),
            "provider_window_full": self.provider_frees.as_ref().map(|(window, frees_at)| json!({
                "window": window.as_str(),
                "frees_at": frees_at,
            })),
            "extra_starts": self.extra_starts,
            "next_start_dollars": self.next_start_dollars,
        })
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(
            text,
            "{:<8} {:>9} {:>14} {:>9}  boatparty's next start leaves",
            "window", "boatparty", "account", "elsewhere"
        );
        for view in &self.windows {
            let account = view.account.map_or_else(
                || "-".to_string(),
                |usage| format!("{}/{}", usage.used, usage.limit),
            );
            let outside = view
                .outside
                .map_or_else(|| "-".to_string(), |outside| outside.to_string());
            let frees = view
                .next_frees_at
                .map_or_else(|| "-".to_string(), |at| self.relative(at));
            let _ = writeln!(
                text,
                "{:<8} {:>9} {:>14} {:>9}  {frees}",
                view.window.as_str(),
                view.boatparty,
                account,
                outside
            );
        }
        let _ = writeln!(
            text,
            "(account: every start boat counted; elsewhere: those boatparty did not make, such as CI or other tools)"
        );
        if let Some((window, frees_at)) = &self.provider_frees {
            let _ = writeln!(
                text,
                "boat's {} window is full until {frees_at}",
                window.as_str()
            );
        }
        if self.extra_starts {
            let price = self
                .next_start_dollars
                .filter(|dollars| *dollars > 0.0)
                .map(|dollars| format!(": the next start costs ${dollars:.2}"))
                .unwrap_or_default();
            let _ = writeln!(text, "extra starts are on{price}");
        }

        let day = &self.windows[2];
        if !day.projects.is_empty() {
            let _ = writeln!(text);
            let _ = writeln!(
                text,
                "{:<24} {:>6} {:>6} {:>6}",
                "project", "1m", "1h", "24h"
            );
            for (project, count) in &day.projects {
                let in_window = |index: usize| {
                    self.windows[index]
                        .projects
                        .get(project)
                        .copied()
                        .unwrap_or(0)
                };
                let _ = writeln!(
                    text,
                    "{project:<24} {:>6} {:>6} {count:>6}",
                    in_window(0),
                    in_window(1)
                );
            }
        }

        if !self.starts.is_empty() {
            let _ = writeln!(text);
            // A compact timeline: starts per hour ago, newest last.
            let mut per_hour = [0usize; 24];
            for (at, _, _) in &self.starts {
                let ago = ((self.now.saturating_sub(*at)) / HOUR_MS).min(23) as usize;
                per_hour[23 - ago] += 1;
            }
            let row: Vec<String> = per_hour.iter().map(ToString::to_string).collect();
            let _ = writeln!(text, "starts per hour, 24h ago to now: {}", row.join(" "));
            let shown = self.starts.len().min(TEXT_TIMELINE);
            let _ = writeln!(text, "the last {shown} of {} (UTC):", self.starts.len());
            for (at, project, in_flight) in &self.starts[self.starts.len() - shown..] {
                let _ = writeln!(
                    text,
                    "  {}  {project}{}",
                    rfc3339(*at),
                    if *in_flight { "  (in flight)" } else { "" }
                );
            }

            let _ = writeln!(text);
            let mut total = 0;
            let mut steps = Vec::new();
            for (hour, frees) in self.frees_by_hour.iter().enumerate() {
                total += frees;
                if [0, 1, 3, 5, 11, 23].contains(&hour) {
                    steps.push(format!("+{}h {total}", hour + 1));
                }
            }
            let _ = writeln!(
                text,
                "boatparty's starts leaving the 24h window (cumulative): {}",
                steps.join(", ")
            );
        } else {
            let _ = writeln!(text);
            let _ = writeln!(text, "boatparty made no start in the last 24 hours");
        }
        text
    }

    fn relative(&self, at: u64) -> String {
        let left = at.saturating_sub(self.now) / 1000;
        if left < 60 {
            format!("in {left}s")
        } else if left < 3600 {
            format!("in {}m", left / 60)
        } else {
            format!("in {}h{:02}m", left / 3600, (left % 3600) / 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{OveragePrices, StartUsage};

    const NOW: u64 = 1_794_736_000_000;

    fn start(project: &str, ago: u64, done: bool) -> StartRecord {
        StartRecord {
            at: NOW - ago,
            project: project.to_string(),
            id: None,
            pid: (!done).then_some(1),
            done_at: done.then_some(NOW - ago + 1_000),
        }
    }

    fn limits(minute: u64, hour: u64, day: u64) -> AccountLimits {
        let window = |used: u64, limit: u64| {
            Some(WindowUsage {
                limit,
                used,
                remaining: limit.saturating_sub(used),
            })
        };
        AccountLimits {
            can_start: true,
            start_blocked_reason: None,
            starts: StartUsage {
                unlimited: false,
                minute: window(minute, 12),
                hour: window(hour, 60),
                day: window(day, 200),
            },
            window_full: None,
            running_sandboxes: 0,
            max_running_sandboxes: 100,
            included: None,
            sizes: Vec::new(),
            overage: OveragePrices::default(),
            next_start_dollars: Some(0.0),
            extra_starts: false,
        }
    }

    #[test]
    fn windows_split_by_project_and_the_gap_is_starts_made_elsewhere() {
        let ledger = [
            start("app", 30_000, false),
            start("app", 20 * MINUTE_MS, true),
            start("web", 50 * MINUTE_MS, true),
            start("app", 3 * HOUR_MS, true),
            start("web", 23 * HOUR_MS + 30 * MINUTE_MS, true),
            // Older than a day: gone.
            start("app", DAY_MS + 1, true),
        ];
        let view = LedgerView::build(&ledger, Some(&limits(1, 10, 40)), NOW);
        let counts: Vec<usize> = view.windows.iter().map(|view| view.boatparty).collect();
        assert_eq!(counts, [1, 3, 5]);
        assert_eq!(view.windows[1].projects["app"], 2);
        assert_eq!(view.windows[1].projects["web"], 1);
        assert_eq!(view.windows[2].projects["app"], 3);
        let outside: Vec<Option<u64>> = view.windows.iter().map(|view| view.outside).collect();
        assert_eq!(outside, [Some(0), Some(7), Some(35)]);
        // The oldest start in each window leaves it first.
        assert_eq!(
            view.windows[0].next_frees_at,
            Some(NOW - 30_000 + MINUTE_MS)
        );
        assert_eq!(
            view.windows[1].next_frees_at,
            Some(NOW - 50 * MINUTE_MS + HOUR_MS)
        );
        assert_eq!(
            view.windows[2].next_frees_at,
            Some(NOW - 23 * HOUR_MS - 30 * MINUTE_MS + DAY_MS)
        );
        // The day-old start leaves within the first hour, the 3 h one in
        // hour 21, the rest in hour 24.
        assert_eq!(view.frees_by_hour.len(), 24);
        assert_eq!(view.frees_by_hour[0], 1);
        assert_eq!(view.frees_by_hour[20], 1);
        assert_eq!(view.frees_by_hour[23], 3);
        assert_eq!(view.frees_by_hour.iter().sum::<usize>(), 5);
        assert_eq!(view.starts.len(), 5);
        assert!(view.starts.last().unwrap().2, "the newest is in flight");
        let text = view.text();
        assert!(text.contains("+24h 5"), "{text}");
        assert!(text.contains("(in flight)"), "{text}");
    }

    #[test]
    fn without_provider_figures_only_the_ledger_shows() {
        let view = LedgerView::build(&[], None, NOW);
        assert!(view.windows.iter().all(|view| view.boatparty == 0
            && view.account.is_none()
            && view.outside.is_none()
            && view.next_frees_at.is_none()));
        assert!(view.text().contains("no start in the last 24 hours"));
        let json = view.json();
        assert_eq!(json["windows"][2]["window"], "day");
        assert_eq!(json["starts"], json!([]));
    }
}
