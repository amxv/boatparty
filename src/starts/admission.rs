//! Admission of one start: a pure function of the ledger, the limits, the
//! cached provider figures and the time. It never touches the state file,
//! the provider or the clock.
//!
//! A start is admitted when the project's own caps (`starts_per_hour`,
//! `starts_per_day`) and the account's starts left in every provider window
//! (minus the reserve) all have room. Otherwise it waits for the longest
//! blocking window, except that a full day window refuses at once.

use super::cache::LimitsCache;
use crate::provider::StartWindow;
use crate::state::StartRecord;

pub const MINUTE_MS: u64 = 60_000;
pub const HOUR_MS: u64 = 60 * MINUTE_MS;
pub const DAY_MS: u64 = 24 * HOUR_MS;
/// Waits end this long after a window frees, so the start lands inside it.
pub const MARGIN_MS: u64 = 250;

pub fn span_ms(window: StartWindow) -> u64 {
    match window {
        StartWindow::Minute => MINUTE_MS,
        StartWindow::Hour => HOUR_MS,
        StartWindow::Day => DAY_MS,
    }
}

/// The start limits of one project and the account's reserve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub project_per_hour: Option<u32>,
    pub project_per_day: Option<u32>,
    pub reserve_per_hour: u32,
    pub reserve_per_day: u32,
    /// `[account] paid_starts`: boatparty may start past the account's
    /// included starts when the provider bills extra starts.
    pub paid_starts: bool,
}

/// Whether the account's own windows (and the reserves kept in them) hold a
/// start back. Not when the provider bills starts past them and the account
/// lets boatparty use that: the provider's hard caps still refuse.
fn account_windows_apply(budget: &Budget, cache: &LimitsCache) -> bool {
    !(budget.paid_starts && cache.extra_starts)
}

/// Whether a start now would be paid: the provider bills extra starts, the
/// account allows them, and one of its windows has no included start left.
pub fn paid_start(ledger: &[StartRecord], budget: &Budget, cache: &LimitsCache, now: u64) -> bool {
    if account_windows_apply(budget, cache) || cache.unlimited {
        return false;
    }
    [StartWindow::Minute, StartWindow::Hour, StartWindow::Day]
        .into_iter()
        .any(|window| {
            let Some(figures) = cache.window(window) else {
                return false;
            };
            let span = span_ms(window);
            if cache.fetched_at + span <= now {
                return false;
            }
            let unseen = ledger
                .iter()
                .filter(|record| record.at + span > now && !reflected(record, cache.fetched_at))
                .count() as u64;
            figures.remaining.saturating_sub(unseen) == 0
        })
}

/// The limit that holds a start back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// The project's own cap, `starts_per_hour` or `starts_per_day`.
    Project { window: StartWindow, cap: u32 },
    /// The account's starts at the provider: `used` counts the provider's
    /// figure plus starts it had not seen yet; `reserve` is kept free.
    Account {
        window: StartWindow,
        limit: u64,
        used: u64,
        reserve: u32,
    },
}

impl Limit {
    pub fn window(&self) -> StartWindow {
        match *self {
            Self::Project { window, .. } | Self::Account { window, .. } => window,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Reserve the start now.
    Admit,
    /// Wait about `wait_ms` (at most), then ask again.
    Wait { limit: Limit, wait_ms: u64 },
    /// The day window is full: fail. `frees_in_ms` is when it has room
    /// again, when known.
    Refuse {
        limit: Limit,
        frees_in_ms: Option<u64>,
    },
}

/// Whether a ledger entry is already inside the provider's figures: its
/// answer arrived before the figures were requested.
fn reflected(record: &StartRecord, fetched_at: u64) -> bool {
    record.done_at.is_some_and(|done| done < fetched_at)
}

/// Decide whether `project` may start one more sandbox at `now` (ms).
pub fn admit(
    ledger: &[StartRecord],
    project: &str,
    budget: &Budget,
    account: Option<&LimitsCache>,
    now: u64,
) -> Admission {
    let mut wait: Option<(Limit, u64)> = None;
    let mut refuse: Option<(Limit, Option<u64>)> = None;
    let mut block = |limit: Limit, frees_in: Option<u64>| {
        if limit.window() == StartWindow::Day {
            if refuse.is_none() {
                refuse = Some((limit, frees_in));
            }
        } else {
            let frees_in = frees_in.unwrap_or(MINUTE_MS);
            if wait.is_none_or(|(_, longest)| frees_in > longest) {
                wait = Some((limit, frees_in));
            }
        }
    };

    for (window, cap) in [
        (StartWindow::Hour, budget.project_per_hour),
        (StartWindow::Day, budget.project_per_day),
    ] {
        let Some(cap) = cap else { continue };
        let span = span_ms(window);
        let mut times: Vec<u64> = ledger
            .iter()
            .filter(|record| record.project == project && record.at + span > now)
            .map(|record| record.at)
            .collect();
        if times.len() < cap as usize {
            continue;
        }
        times.sort_unstable();
        // The window has room once its oldest counted start ages out.
        let oldest = times[times.len() - cap as usize];
        block(
            Limit::Project { window, cap },
            Some((oldest + span + MARGIN_MS).saturating_sub(now)),
        );
    }

    if let Some(cache) =
        account.filter(|cache| !cache.unlimited && account_windows_apply(budget, cache))
    {
        for (window, reserve) in [
            (StartWindow::Minute, 0),
            (StartWindow::Hour, budget.reserve_per_hour),
            (StartWindow::Day, budget.reserve_per_day),
        ] {
            let Some(figures) = cache.window(window) else {
                continue;
            };
            let span = span_ms(window);
            // Every start in the figures has aged out by now.
            if cache.fetched_at + span <= now {
                continue;
            }
            let unseen = ledger
                .iter()
                .filter(|record| record.at + span > now && !reflected(record, cache.fetched_at))
                .count() as u64;
            let left = figures.remaining.saturating_sub(unseen);
            if left > u64::from(reserve) {
                continue;
            }
            // At the latest, every start in the figures has aged out; the
            // provider's own date is better when only its figure is full.
            let mut frees_in = (cache.fetched_at + span + MARGIN_MS).saturating_sub(now);
            if unseen == 0
                && reserve == 0
                && let Some(at) = cache.frees_at(window).filter(|at| *at > now)
            {
                frees_in = frees_in.min(at + MARGIN_MS - now);
            }
            block(
                Limit::Account {
                    window,
                    limit: figures.limit,
                    used: figures.used + unseen,
                    reserve,
                },
                Some(frees_in),
            );
        }
    }

    match (refuse, wait) {
        (Some((limit, frees_in_ms)), _) => Admission::Refuse { limit, frees_in_ms },
        (None, Some((limit, wait_ms))) => Admission::Wait { limit, wait_ms },
        (None, None) => Admission::Admit,
    }
}

/// Drop ledger entries older than a day and in-flight reservations whose
/// process is gone.
pub fn prune(ledger: &mut Vec<StartRecord>, now: u64, alive: impl Fn(u32) -> bool) {
    ledger.retain(|record| {
        record.at + DAY_MS > now && (record.done_at.is_some() || record.pid.is_none_or(&alive))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::starts::cache::CachedWindow;

    const NOW: u64 = 100 * DAY_MS;

    fn start(project: &str, ago: u64) -> StartRecord {
        StartRecord {
            at: NOW - ago,
            project: project.into(),
            id: None,
            pid: None,
            done_at: Some(NOW - ago + 1_000),
        }
    }

    fn cache(
        fetched_ago: u64,
        minute: (u64, u64),
        hour: (u64, u64),
        day: (u64, u64),
    ) -> LimitsCache {
        let window = |(limit, used): (u64, u64)| {
            Some(CachedWindow {
                limit,
                used,
                remaining: limit - used,
            })
        };
        LimitsCache {
            fetched_at: NOW - fetched_ago,
            unlimited: false,
            minute: window(minute),
            hour: window(hour),
            day: window(day),
            running: 0,
            max_running: 100,
            window_full: None,
            extra_starts: false,
            next_start_mills: None,
        }
    }

    const ROOMY: ((u64, u64), (u64, u64), (u64, u64)) = ((12, 0), (60, 0), (200, 0));

    #[test]
    fn extra_starts_let_a_full_account_window_start_as_paid_unless_opted_out() {
        let mut full = cache(1_000, (12, 0), (60, 25), (200, 200));
        let ledger = [start("app", 10 * MINUTE_MS)];
        let paying = Budget {
            paid_starts: true,
            reserve_per_day: 10,
            ..Budget::default()
        };
        // Extra starts off at the provider: refused, as before.
        assert!(matches!(
            admit(&ledger, "app", &paying, Some(&full), NOW),
            Admission::Refuse { .. }
        ));
        assert!(!paid_start(&ledger, &paying, &full, NOW));
        // On: admitted, and known to be paid.
        full.extra_starts = true;
        assert_eq!(
            admit(&ledger, "app", &paying, Some(&full), NOW),
            Admission::Admit
        );
        assert!(paid_start(&ledger, &paying, &full, NOW));
        // Opted out in account.toml: refused.
        let strict = Budget {
            paid_starts: false,
            ..paying
        };
        assert!(matches!(
            admit(&ledger, "app", &strict, Some(&full), NOW),
            Admission::Refuse { .. }
        ));
        assert!(!paid_start(&ledger, &strict, &full, NOW));
        // Inside the included starts nothing is paid.
        let mut roomy = cache(1_000, ROOMY.0, ROOMY.1, ROOMY.2);
        roomy.extra_starts = true;
        assert!(!paid_start(&ledger, &paying, &roomy, NOW));
        // The project's own caps still hold.
        let capped = Budget {
            project_per_day: Some(1),
            ..paying
        };
        assert!(matches!(
            admit(&ledger, "app", &capped, Some(&full), NOW),
            Admission::Refuse {
                limit: Limit::Project { .. },
                ..
            }
        ));
    }

    #[test]
    fn nothing_blocks_an_empty_ledger() {
        let fresh = cache(1_000, ROOMY.0, ROOMY.1, ROOMY.2);
        assert_eq!(
            admit(&[], "app", &Budget::default(), Some(&fresh), NOW),
            Admission::Admit
        );
        assert_eq!(
            admit(&[], "app", &Budget::default(), None, NOW),
            Admission::Admit
        );
    }

    #[test]
    fn the_minute_window_waits_until_the_figures_age_out() {
        // Fetched 20 s ago with the minute full: wait the other 40 s.
        let full = cache(20_000, (12, 12), (60, 12), (200, 12));
        let Admission::Wait { limit, wait_ms } =
            admit(&[], "app", &Budget::default(), Some(&full), NOW)
        else {
            panic!("expected a wait");
        };
        assert_eq!(
            limit,
            Limit::Account {
                window: StartWindow::Minute,
                limit: 12,
                used: 12,
                reserve: 0
            }
        );
        assert_eq!(wait_ms, 40_000 + MARGIN_MS);
        // The provider's own date is used when it is sooner.
        let mut dated = full.clone();
        dated.window_full = Some(super::super::cache::CachedFull {
            window: "minute".into(),
            frees_at: Some(NOW + 5_000),
        });
        assert_eq!(
            admit(&[], "app", &Budget::default(), Some(&dated), NOW),
            Admission::Wait {
                limit,
                wait_ms: 5_000 + MARGIN_MS
            }
        );
        // A minute later those figures say nothing any more.
        let old = cache(61_000, (12, 12), (60, 12), (200, 12));
        assert_eq!(
            admit(&[], "app", &Budget::default(), Some(&old), NOW),
            Admission::Admit
        );
    }

    #[test]
    fn the_hour_window_counts_starts_the_figures_have_not_seen() {
        // 58 of 60 used when fetched 10 minutes ago, then two more starts
        // here: one answered after the fetch, one still in flight.
        let figures = cache(10 * MINUTE_MS, (12, 0), (60, 58), (200, 58));
        let mut late = start("other", 5 * MINUTE_MS);
        late.done_at = Some(NOW - 4 * MINUTE_MS);
        let mut flying = start("app", 1_000);
        flying.done_at = None;
        // An old start the figures already include does not count twice.
        let seen = start("app", 30 * MINUTE_MS);
        let ledger = [seen, late.clone()];
        assert_eq!(
            admit(&ledger, "app", &Budget::default(), Some(&figures), NOW),
            Admission::Admit
        );
        let ledger = [late, flying];
        let Admission::Wait { limit, wait_ms } =
            admit(&ledger, "app", &Budget::default(), Some(&figures), NOW)
        else {
            panic!("expected a wait");
        };
        assert_eq!(
            limit,
            Limit::Account {
                window: StartWindow::Hour,
                limit: 60,
                used: 60,
                reserve: 0
            }
        );
        assert_eq!(wait_ms, 50 * MINUTE_MS + MARGIN_MS);
    }

    #[test]
    fn the_account_reserve_is_left_unused() {
        let figures = cache(1_000, (12, 0), (60, 55), (200, 55));
        let budget = Budget {
            reserve_per_hour: 5,
            ..Budget::default()
        };
        let Admission::Wait { limit, .. } = admit(&[], "app", &budget, Some(&figures), NOW) else {
            panic!("expected a wait");
        };
        assert_eq!(
            limit,
            Limit::Account {
                window: StartWindow::Hour,
                limit: 60,
                used: 55,
                reserve: 5
            }
        );
        let budget = Budget {
            reserve_per_hour: 4,
            ..Budget::default()
        };
        assert_eq!(
            admit(&[], "app", &budget, Some(&figures), NOW),
            Admission::Admit
        );
        // The day reserve refuses instead of waiting.
        let budget = Budget {
            reserve_per_day: 145,
            ..Budget::default()
        };
        assert!(matches!(
            admit(&[], "app", &budget, Some(&figures), NOW),
            Admission::Refuse {
                limit: Limit::Account {
                    window: StartWindow::Day,
                    reserve: 145,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn a_project_hour_cap_waits_for_its_oldest_start() {
        let budget = Budget {
            project_per_hour: Some(2),
            ..Budget::default()
        };
        // Another project's starts do not count against this project.
        let ledger = [
            start("app", 50 * MINUTE_MS),
            start("app", 10 * MINUTE_MS),
            start("other", MINUTE_MS),
        ];
        assert_eq!(
            admit(&ledger, "app", &budget, None, NOW),
            Admission::Wait {
                limit: Limit::Project {
                    window: StartWindow::Hour,
                    cap: 2
                },
                wait_ms: 10 * MINUTE_MS + MARGIN_MS
            }
        );
        assert_eq!(
            admit(&ledger, "other", &budget, None, NOW),
            Admission::Admit
        );
        assert_eq!(
            admit(&ledger[1..], "app", &budget, None, NOW),
            Admission::Admit
        );
    }

    #[test]
    fn a_full_day_refuses_and_says_when_it_frees() {
        let budget = Budget {
            project_per_hour: Some(1),
            project_per_day: Some(2),
            ..Budget::default()
        };
        let ledger = [start("app", 20 * HOUR_MS), start("app", 30 * MINUTE_MS)];
        // The day refusal wins over the hour's wait.
        assert_eq!(
            admit(&ledger, "app", &budget, None, NOW),
            Admission::Refuse {
                limit: Limit::Project {
                    window: StartWindow::Day,
                    cap: 2
                },
                frees_in_ms: Some(4 * HOUR_MS + MARGIN_MS)
            }
        );
        let used_up = cache(1_000, (12, 0), (60, 10), (200, 200));
        assert!(matches!(
            admit(&[], "app", &Budget::default(), Some(&used_up), NOW),
            Admission::Refuse {
                limit: Limit::Account {
                    window: StartWindow::Day,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn an_unlimited_account_has_no_account_windows() {
        let mut unlimited = cache(1_000, (12, 12), (60, 60), (200, 200));
        unlimited.unlimited = true;
        assert_eq!(
            admit(&[], "app", &Budget::default(), Some(&unlimited), NOW),
            Admission::Admit
        );
    }

    #[test]
    fn pruning_drops_old_entries_and_dead_reservations() {
        let mut flying_dead = start("app", 1_000);
        flying_dead.done_at = None;
        flying_dead.pid = Some(1);
        let mut flying_alive = flying_dead.clone();
        flying_alive.pid = Some(2);
        let mut ledger = vec![
            start("app", DAY_MS + 1),
            start("app", DAY_MS - 1_000),
            flying_dead,
            flying_alive.clone(),
        ];
        prune(&mut ledger, NOW, |pid| pid == 2);
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger[1], flying_alive);
    }
}
