//! The `StartLedger`: every create and resume, of every project, is admitted
//! here before it is sent.
//!
//! Under the state lock a start is reserved (a ledger entry tagged with the
//! project) when [`admission::admit`] allows it; otherwise the caller waits,
//! with one log line naming the limit. The provider's start figures are
//! cached in `state.json` for a few minutes and fetched again after every
//! counted start, so the ledger tracks the provider's own count. A start the
//! provider refuses, or does not count (a resume that takes back its kept
//! machine), gives its reservation back. A rate-limited start is retried
//! after the provider's own delay; only the day window fails.

pub mod admission;
pub mod cache;

use std::time::{Duration, Instant};

use admission::{Admission, Budget, Limit};
use cache::LimitsCache;

use crate::account::ProjectSettings;
use crate::error::AppError;
use crate::output::Output;
use crate::paths::Paths;
use crate::provider::{ErrorKind, Provider, ProviderError, StartRefusal, StartWindow};
use crate::runs::parse_rfc3339;
use crate::state::{StartRecord, State, Store};
use crate::system;

/// Cached provider figures older than this are fetched again before a start.
pub const CACHE_TTL: Duration = Duration::from_secs(180);
/// While the account's figures hold a start back, fetch them at most this often.
const RECHECK: Duration = Duration::from_secs(60);
/// The longest single sleep while waiting, so a waiting process notices
/// changes (another project's reservation given back, an edited limit).
const WAIT_STEP: Duration = Duration::from_secs(15);
/// The delay after a refusal that named no delay.
const DEFAULT_RETRY: Duration = Duration::from_secs(30);

impl Budget {
    pub fn from_settings(settings: &ProjectSettings) -> Self {
        Self {
            project_per_hour: settings.starts_per_hour,
            project_per_day: settings.starts_per_day,
            reserve_per_hour: settings.reserve_starts_per_hour,
            reserve_per_day: settings.reserve_starts_per_day,
        }
    }
}

/// Why a start did not happen.
#[derive(Debug)]
pub enum StartFailure {
    /// The provider answered the start with an error. Whether a start
    /// happened follows from its kind, as without the gate.
    Provider(ProviderError),
    /// Refused before or instead of starting (the day window), or the state
    /// file failed before the call.
    Refused(AppError),
}

impl From<StartFailure> for AppError {
    fn from(failure: StartFailure) -> Self {
        match failure {
            StartFailure::Provider(error) => error.into(),
            StartFailure::Refused(error) => error,
        }
    }
}

/// Admits one project's starts.
pub struct StartGate<'a> {
    pub paths: &'a Paths,
    pub store: &'a Store,
    pub provider: &'a dyn Provider,
    pub out: Output,
    pub project: &'a str,
    pub budget: Budget,
}

/// Whether an error answer means the provider started nothing. Only a lost
/// or unreadable answer leaves that open.
fn nothing_started(kind: &ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Transport | ErrorKind::Server | ErrorKind::InvalidResponse | ErrorKind::Unknown
    )
}

impl StartGate<'_> {
    /// Run `call` (one create or resume) under a ledger reservation, waiting
    /// for room first. `counted` says whether a successful answer spent a
    /// start; when it did not, the reservation is given back.
    pub fn start<T>(
        &self,
        what: &str,
        mut call: impl FnMut() -> Result<T, ProviderError>,
        counted: impl Fn(&T) -> bool,
    ) -> Result<T, StartFailure> {
        let mut force_refresh = false;
        loop {
            let id = self
                .reserve(what, std::mem::take(&mut force_refresh))
                .map_err(StartFailure::Refused)?;
            match call() {
                Ok(value) => {
                    if counted(&value) {
                        self.settle(&id, true);
                        self.refresh_in_background();
                    } else {
                        self.settle(&id, false);
                    }
                    return Ok(value);
                }
                Err(error) => match error.kind() {
                    ErrorKind::RateLimited(refusal) => {
                        // A refused start does not count.
                        self.settle(&id, false);
                        if refusal.window == Some(StartWindow::Day) {
                            return Err(StartFailure::Refused(provider_day_error(&error, refusal)));
                        }
                        let delay = provider_delay(refusal, system::now_ms());
                        self.out.note(format_args!(
                            "the provider refused starting {what}: its {} start limit{} is full; \
                             retrying in {}",
                            refusal.window.map_or("rolling", StartWindow::as_str),
                            refusal
                                .limit
                                .map(|limit| format!(" ({limit})"))
                                .unwrap_or_default(),
                            duration_words(delay)
                        ));
                        std::thread::sleep(delay);
                        force_refresh = true;
                    }
                    kind if nothing_started(kind) => {
                        self.settle(&id, false);
                        return Err(StartFailure::Provider(error));
                    }
                    _ => {
                        // The answer was lost: count it until the next fetch says.
                        self.settle(&id, true);
                        return Err(StartFailure::Provider(error));
                    }
                },
            }
        }
    }

    /// Wait until a start is admitted and reserve it. Returns its id.
    fn reserve(&self, what: &str, mut force_refresh: bool) -> Result<String, AppError> {
        let id = system::random_hex(8)?;
        let pid = std::process::id();
        let mut announced: Option<String> = None;
        let mut fetched: Option<Instant> = None;
        loop {
            let cache_age = self.cache_age()?;
            let stale = cache_age.is_none_or(|age| age >= CACHE_TTL);
            if (force_refresh || stale) && fetched.is_none_or(|at| at.elapsed() >= RECHECK) {
                fetched = Some(Instant::now());
                force_refresh = false;
                refresh(self.store, self.provider);
            }
            let outcome = self.store.update(|state| {
                let now = system::now_ms();
                admission::prune(&mut state.starts, now, system::pid_alive);
                let cache = LimitsCache::read(state);
                let admission = admission::admit(
                    &state.starts,
                    self.project,
                    &self.budget,
                    cache.as_ref(),
                    now,
                );
                if admission == Admission::Admit {
                    state.starts.push(StartRecord {
                        at: now,
                        project: self.project.to_string(),
                        id: Some(id.clone()),
                        pid: Some(pid),
                        done_at: None,
                    });
                }
                Ok(admission)
            })?;
            let (limit, wait_ms) = match outcome {
                Admission::Admit => return Ok(id),
                Admission::Refuse { limit, frees_in_ms } => {
                    // Figures that may be old get one more look first.
                    if matches!(limit, Limit::Account { .. })
                        && fetched.is_none_or(|at| at.elapsed() >= RECHECK)
                        && self.cache_age()?.is_none_or(|age| age >= RECHECK)
                    {
                        force_refresh = true;
                        continue;
                    }
                    return Err(self.day_error(limit, frees_in_ms));
                }
                Admission::Wait { limit, wait_ms } => (limit, wait_ms),
            };
            if matches!(limit, Limit::Account { .. })
                && fetched.is_none_or(|at| at.elapsed() >= RECHECK)
                && self.cache_age()?.is_none_or(|age| age >= RECHECK)
            {
                force_refresh = true;
                continue;
            }
            let reason = self.describe(limit);
            if announced.as_deref() != Some(reason.as_str()) {
                let wait = Duration::from_millis(wait_ms);
                let estimate = match limit {
                    Limit::Project { .. } => format!("about {}", duration_words(wait)),
                    Limit::Account { .. } => format!("up to {}", duration_words(wait)),
                };
                self.out.note(format_args!(
                    "starting {what} waits for {reason}: {estimate}"
                ));
                announced = Some(reason);
            }
            std::thread::sleep(
                Duration::from_millis(wait_ms).clamp(Duration::from_millis(10), WAIT_STEP),
            );
        }
    }

    /// Keep (`counted`) or give back a reservation. Ledger failures after
    /// the call are only reported: the start itself already happened.
    fn settle(&self, id: &str, counted: bool) {
        let result = self.store.update(|state| {
            let now = system::now_ms();
            if counted {
                if let Some(record) = state
                    .starts
                    .iter_mut()
                    .find(|record| record.id.as_deref() == Some(id))
                {
                    record.done_at = Some(now);
                    record.pid = None;
                }
            } else {
                state
                    .starts
                    .retain(|record| record.id.as_deref() != Some(id));
            }
            Ok(())
        });
        if let Err(error) = result {
            self.out.note(format_args!(
                "warning: could not update the start ledger: {error}"
            ));
        }
    }

    fn cache_age(&self) -> Result<Option<Duration>, AppError> {
        let state = self.store.load()?;
        Ok(LimitsCache::read(&state)
            .map(|cache| Duration::from_millis(system::now_ms().saturating_sub(cache.fetched_at))))
    }

    /// Fetch the figures again after a counted start, without holding up
    /// the caller (a lost fetch only means the next start fetches them).
    fn refresh_in_background(&self) {
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            if let Ok(provider) = crate::pool::lease::open_provider(&paths) {
                refresh(&Store::new(&paths), provider.as_ref());
            }
        });
    }

    fn account_file(&self) -> String {
        self.paths.account_file().display().to_string()
    }

    /// The limit in words, for the wait line.
    fn describe(&self, limit: Limit) -> String {
        match limit {
            Limit::Project { window, cap } => format!(
                "project {}'s {} start limit ({} = {cap} under [projects.{}] in {})",
                self.project,
                per(window),
                project_key(window),
                self.project,
                self.account_file()
            ),
            Limit::Account {
                window,
                limit,
                used,
                reserve,
            } => {
                let kept = if reserve > 0 {
                    format!(
                        ", {reserve} kept in reserve (reserve_starts_per_{})",
                        window.as_str()
                    )
                } else {
                    String::new()
                };
                format!(
                    "the account's {} starts ({used} of {limit} used{kept})",
                    per(window)
                )
            }
        }
    }

    fn day_error(&self, limit: Limit, frees_in_ms: Option<u64>) -> AppError {
        let when = frees_in_ms
            .map(|ms| {
                format!(
                    "; the next start is possible in about {}",
                    duration_words(Duration::from_millis(ms))
                )
            })
            .unwrap_or_default();
        match limit {
            Limit::Project { cap, .. } => AppError::new(
                "daily_start_limit",
                format!(
                    "project {} reached its daily start limit: {cap} starts in the last 24 hours \
                     (starts_per_day = {cap} under [projects.{}] in {}){when}",
                    self.project,
                    self.project,
                    self.account_file()
                ),
            )
            .with_hint(format!(
                "running sandboxes keep working; raise it with `boatparty limits set project {} \
                 starts_per_day=N`",
                self.project
            )),
            Limit::Account {
                limit,
                used,
                reserve,
                ..
            } => AppError::new(
                "daily_start_limit",
                format!(
                    "the account's daily start limit leaves boatparty no start: {used} of {limit} \
                     starts used in the last 24 hours, {reserve} kept in reserve \
                     (reserve_starts_per_day in {}){when}",
                    self.account_file()
                ),
            )
            .with_hint(
                "running sandboxes keep working; boatparty never pays for extra starts, so wait, \
                 lower the reserve with `boatparty limits set account reserve_starts_per_day=N`, \
                 or raise the plan",
            ),
        }
    }
}

/// Fetch the provider's figures and store them. Failures leave the cache
/// as it was: the provider's own refusals still guard every start.
pub fn refresh(store: &Store, provider: &dyn Provider) -> Option<LimitsCache> {
    let requested_at = system::now_ms();
    let limits = provider.limits().ok()?;
    let cache = LimitsCache::from_limits(&limits, requested_at);
    store
        .update(|state| {
            // Keep a newer fetch made meanwhile by another process.
            if LimitsCache::read(state).is_none_or(|old| old.fetched_at <= requested_at) {
                cache.write(state);
            }
            Ok(())
        })
        .ok()?;
    Some(cache)
}

/// Starts of `project` in the ledger within each window: (hour, day).
pub fn project_usage(state: &State, project: &str, now: u64) -> (usize, usize) {
    let count = |span: u64| {
        state
            .starts
            .iter()
            .filter(|record| record.project == project && record.at + span > now)
            .count()
    };
    (count(admission::HOUR_MS), count(admission::DAY_MS))
}

/// How long the provider asked to wait: its delay, else until its next
/// start time, else a default; plus a little so the retry lands after it.
fn provider_delay(refusal: &StartRefusal, now: u64) -> Duration {
    let named = refusal.retry_after.or_else(|| {
        let at = refusal.next_start_at.as_deref().and_then(parse_rfc3339)?;
        Some(Duration::from_millis(at.saturating_sub(now)))
    });
    named.unwrap_or(DEFAULT_RETRY) + Duration::from_millis(500)
}

fn provider_day_error(error: &ProviderError, refusal: &StartRefusal) -> AppError {
    let limit = refusal
        .limit
        .map(|limit| format!(" ({limit} starts)"))
        .unwrap_or_default();
    let next = refusal
        .next_start_at
        .as_deref()
        .map(|at| format!("; the next start is possible at {at}"))
        .unwrap_or_default();
    AppError::new(
        "daily_start_limit",
        format!("the provider's daily start limit{limit} is full{next} ({error})"),
    )
    .with_hint("running sandboxes keep working; boatparty never pays for extra starts")
}

fn per(window: StartWindow) -> &'static str {
    match window {
        StartWindow::Minute => "per-minute",
        StartWindow::Hour => "hourly",
        StartWindow::Day => "daily",
    }
}

fn project_key(window: StartWindow) -> &'static str {
    match window {
        StartWindow::Day => "starts_per_day",
        _ => "starts_per_hour",
    }
}

/// `1h 05m`, `4m 10s`, `12s`.
pub fn duration_words(duration: Duration) -> String {
    let secs = duration.as_secs() + u64::from(duration.subsec_millis() >= 500);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_well() {
        assert_eq!(duration_words(Duration::from_millis(12_400)), "12s");
        assert_eq!(duration_words(Duration::from_secs(250)), "4m 10s");
        assert_eq!(duration_words(Duration::from_secs(3_900)), "1h 05m");
    }

    #[test]
    fn the_provider_delay_comes_from_its_answer() {
        let mut refusal = StartRefusal {
            retry_after: Some(Duration::from_secs(41)),
            ..StartRefusal::default()
        };
        assert_eq!(provider_delay(&refusal, 0), Duration::from_millis(41_500));
        refusal.retry_after = None;
        refusal.next_start_at = Some("1970-01-01T00:01:00Z".into());
        assert_eq!(
            provider_delay(&refusal, 30_000),
            Duration::from_millis(30_500)
        );
        refusal.next_start_at = None;
        assert_eq!(
            provider_delay(&refusal, 0),
            DEFAULT_RETRY + Duration::from_millis(500)
        );
    }

    #[test]
    fn only_a_lost_answer_may_have_started_something() {
        assert!(nothing_started(&ErrorKind::NotFound));
        assert!(nothing_started(&ErrorKind::Busy {
            reason: crate::provider::BusyReason::Stopping,
            retry_after: None
        }));
        assert!(!nothing_started(&ErrorKind::Transport));
        assert!(!nothing_started(&ErrorKind::Server));
    }
}
