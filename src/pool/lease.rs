//! Leases and the auto-stop deadline.
//!
//! A process that uses a member registers a lease with its pid. While it
//! holds the lease a background heartbeat pushes the member's auto-stop
//! deadline `lease_minutes` ahead every `heartbeat_minutes`. When it lets go
//! and no other live lease remains, a detached helper process sets the idle
//! deadline (`idle_minutes`, or longer while the member is still hydrating),
//! so the caller exits at once. The provider's auto-stop is the only reaper:
//! boatparty never stops a member by itself here.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::wait;
use crate::account::ProjectSettings;
use crate::error::AppError;
use crate::paths::Paths;
use crate::provider::{self, Provider, SandboxState};
use crate::state::{Lease, LeaseKind, Member, Store};
use crate::system;

/// The hidden subcommand that runs [`idle_helper`].
pub const IDLE_HELPER_COMMAND: &str = "__idle";

/// How long the idle helper waits before looking, so a command that follows
/// at once (scripts often call boatparty in a row) can take a lease first.
const IDLE_DELAY_POLLS: u32 = 2;
/// The idle helper gives up waiting for hydration after this long and sets
/// the idle deadline anyway.
const HYDRATION_WAIT: Duration = Duration::from_secs(30 * 60);
/// How often the idle helper looks at a hydrating member, in polls.
const HYDRATION_POLLS: u32 = 30;
/// A restore that makes no progress (or that the provider reports as
/// failing), or a machine the provider cannot reach, for this long marks the
/// member broken: its next bring-up replaces it.
const STALL_LIMIT: Duration = Duration::from_secs(10 * 60);
/// Overrides [`STALL_LIMIT`] (seconds), for tests and tuning.
pub const STALL_LIMIT_ENV: &str = "BOATPARTY_RESTORE_STALL_SECONDS";

/// Watches one member's restore across the idle helper's looks and says
/// when it has been stuck for too long. Pure: the caller passes the time.
#[derive(Debug, Clone, Default)]
pub struct StallWatch {
    /// When the current stall began, and the progress seen then.
    since: Option<(Instant, Option<u8>)>,
}

impl StallWatch {
    /// Record one look at `sandbox` at `now`. Returns why the member is
    /// broken once a stall has lasted `limit`.
    pub fn look(
        &mut self,
        sandbox: &crate::provider::Sandbox,
        now: Instant,
        limit: Duration,
    ) -> Option<String> {
        let percent = match sandbox.hydration {
            crate::provider::Hydration::InProgress { percent } => percent,
            _ => None,
        };
        let restoring = !sandbox.hydration.is_complete();
        // No progress since the stall began counts as stalled too, when the
        // provider reports progress at all.
        let flat = restoring
            && percent.is_some()
            && self
                .since
                .is_some_and(|(_, seen)| seen.is_some() && seen == percent);
        let stalled = sandbox.restore_stalled || sandbox.degraded_since.is_some() || flat;
        if !stalled {
            // Progress (or nothing to judge): start over from here.
            self.since = restoring.then_some((now, percent));
            return None;
        }
        let (began, _) = *self.since.get_or_insert((now, percent));
        if now.saturating_duration_since(began) < limit {
            return None;
        }
        Some(if sandbox.degraded_since.is_some() {
            format!(
                "the provider could not reach its machine for {} minutes",
                limit.as_secs() / 60
            )
        } else {
            format!(
                "restoring its files after a resume made no progress for {} minutes",
                limit.as_secs() / 60
            )
        })
    }
}

/// Record that the member `member_id` of `project` is broken.
pub fn mark_broken(
    store: &Store,
    project: &str,
    member_id: &str,
    reason: &str,
) -> Result<(), AppError> {
    store.update(|state| {
        if let Some(member) = state
            .project_mut(project)
            .members
            .iter_mut()
            .find(|member| member.id.as_deref() == Some(member_id))
            && member.broken.is_none()
        {
            member.broken = Some(crate::state::Broken {
                reason: reason.to_string(),
                at: system::now_ms(),
            });
        }
        Ok(())
    })
}

/// The deadlines leases work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTimes {
    pub lease_seconds: u64,
    pub idle_seconds: u64,
    pub heartbeat: Duration,
}

impl LeaseTimes {
    /// The project's lease and idle lengths, with a heartbeat that fits
    /// inside its lease.
    pub fn from_settings(settings: &ProjectSettings) -> Self {
        Self {
            lease_seconds: settings.lease_seconds(),
            idle_seconds: settings.idle_seconds(),
            heartbeat: settings.heartbeat(),
        }
    }

    /// The lease times of `project` in `account.toml`.
    pub fn for_project(config: &crate::account::AccountConfig, project: &str) -> Self {
        Self::from_settings(&ProjectSettings::resolve(config, project))
    }
}

/// Add this process's lease to the member (matched by its create key),
/// dropping leases whose process is gone, but only while the member still
/// belongs to `workspace`. Returns false, changing nothing, when the record
/// is gone or now belongs elsewhere: the caller then acquires again.
pub fn register(
    store: &Store,
    project: &str,
    workspace: &str,
    member: &Member,
    kind: LeaseKind,
    label: &str,
) -> Result<bool, AppError> {
    let pid = std::process::id();
    let label: String = label.chars().take(120).collect();
    store.update(|state| {
        let pool = state.project_mut(project);
        let Some(slot) = pool.find_mut_by_key(&member.create_key) else {
            return Ok(false);
        };
        if slot.affinity.as_deref() != Some(workspace) {
            return Ok(false);
        }
        slot.leases
            .retain(|lease| lease.pid != pid && system::pid_alive(lease.pid));
        slot.leases.push(Lease {
            pid,
            kind,
            label,
            started_at: system::now_ms(),
            workspace: Some(workspace.to_string()),
        });
        slot.last_used_at = system::now_ms();
        Ok(true)
    })
}

/// Drop `pid`'s leases from every member of the project and apply `change`
/// to the member with this provider id. Returns how many live leases that
/// member still has.
pub fn release(
    store: &Store,
    project: &str,
    member_id: &str,
    pid: u32,
    change: impl FnOnce(&mut Member),
) -> Result<usize, AppError> {
    store.update(|state| {
        let pool = state.project_mut(project);
        for member in &mut pool.members {
            member
                .leases
                .retain(|lease| lease.pid != pid && system::pid_alive(lease.pid));
        }
        let Some(member) = pool
            .members
            .iter_mut()
            .find(|member| member.id.as_deref() == Some(member_id))
        else {
            return Ok(0);
        };
        change(member);
        Ok(member.leases.len())
    })
}

/// Release this process's lease; when it was the last live one, call
/// `set_idle` (which starts the idle helper). Returns whether it was last.
pub fn finish(
    store: &Store,
    project: &str,
    member_id: &str,
    change: impl FnOnce(&mut Member),
    set_idle: impl FnOnce(),
) -> Result<bool, AppError> {
    let others = release(store, project, member_id, std::process::id(), change)?;
    if others == 0 {
        set_idle();
    }
    Ok(others == 0)
}

/// Whether the member with this provider id has a live lease now.
pub fn has_live_lease(store: &Store, project: &str, member_id: &str) -> Result<bool, AppError> {
    let state = store.load()?;
    Ok(state.project(project).is_some_and(|pool| {
        pool.members
            .iter()
            .any(|member| member.id.as_deref() == Some(member_id) && member.has_live_lease())
    }))
}

/// Pushes the deadline forward now and then on every interval until stopped.
/// Stopping never waits for a push in flight.
pub struct Heartbeat {
    stop: Option<Sender<()>>,
}

impl Heartbeat {
    /// Run the extender `make` builds (in the heartbeat's own thread) now and
    /// then every `interval`.
    pub fn start<F: FnMut()>(
        interval: Duration,
        make: impl FnOnce() -> F + Send + 'static,
    ) -> Self {
        let (stop, stopped) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut extend = make();
            loop {
                extend();
                match stopped.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => continue,
                    _ => break,
                }
            }
        });
        Self { stop: Some(stop) }
    }

    /// A heartbeat that pushes this member's deadline through the provider.
    /// The provider is opened in the background thread, so starting the
    /// heartbeat costs the caller nothing.
    pub fn for_member(paths: &Paths, member_id: &str, times: LeaseTimes) -> Self {
        let paths = paths.clone();
        let id = member_id.to_string();
        Self::start(times.heartbeat, move || {
            let mut provider: Option<Box<dyn Provider>> = None;
            let mut warned = false;
            move || {
                if provider.is_none() {
                    provider = open_provider(&paths).ok();
                }
                let Some(provider) = provider.as_deref() else {
                    return;
                };
                if let Err(error) = provider.set_ttl(&id, times.lease_seconds)
                    && !warned
                {
                    warned = true;
                    eprintln!(
                        "boatparty: warning: could not push the auto-stop deadline of {id} forward: {error}"
                    );
                }
            }
        })
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The provider from the stored account and key, without printing anything.
pub fn open_provider(paths: &Paths) -> Result<Box<dyn Provider>, AppError> {
    let account = crate::account::load_or_create(paths)?;
    let (key, _) = crate::account::credentials::load(paths)?
        .ok_or_else(|| AppError::credentials("no boat API key is stored"))?;
    provider::connect(account.config.provider.api_url.as_deref(), key)
}

/// Start the detached helper that sets the idle deadline. It runs in a
/// process group of its own with no terminal streams; this process does not
/// wait for it.
pub fn spawn_idle_helper(project: &str, member_id: &str) -> Result<(), AppError> {
    let exe: PathBuf = std::env::current_exe()
        .map_err(|error| AppError::io("cannot find the boatparty executable", error))?;
    let mut command = Command::new(exe);
    command
        .args([IDLE_HELPER_COMMAND, project, member_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .spawn()
        .map_err(|error| AppError::io("cannot start the idle helper", error))?;
    Ok(())
}

/// The idle helper: once no live lease remains on the member, set its
/// auto-stop to the idle length; while it is hydrating keep it a lease
/// length ahead instead, so the idle deadline never falls inside a known
/// hydration.
pub fn idle_helper(paths: &Paths, project: &str, member_id: &str) -> Result<(), AppError> {
    std::thread::sleep(wait::poll_interval().saturating_mul(IDLE_DELAY_POLLS));
    let store = Store::new(paths);
    if has_live_lease(&store, project, member_id)? {
        return Ok(());
    }
    let account = crate::account::load_or_create(paths)?;
    let times = LeaseTimes::for_project(&account.config, project);
    let provider = open_provider(paths)?;
    let set_ttl = |seconds: u64| {
        wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.set_ttl(member_id, seconds)
        })
    };
    let deadline = Instant::now() + HYDRATION_WAIT;
    let limit = std::env::var(STALL_LIMIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map_or(STALL_LIMIT, Duration::from_secs);
    let mut watch = StallWatch::default();
    loop {
        if has_live_lease(&store, project, member_id)? {
            return Ok(());
        }
        let sandbox = wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.get(member_id)
        })?;
        if sandbox.state != SandboxState::Running {
            return Ok(());
        }
        let broken = watch.look(&sandbox, Instant::now(), limit);
        if let Some(reason) = &broken {
            // The next bring-up replaces it; meanwhile it may stop.
            mark_broken(&store, project, member_id, reason)?;
        }
        let healthy = sandbox.hydration.is_complete() && sandbox.degraded_since.is_none();
        if healthy || broken.is_some() || Instant::now() >= deadline {
            set_ttl(times.idle_seconds)?;
            // A command that took a lease meanwhile may have pushed its
            // deadline just before this shortened it.
            if has_live_lease(&store, project, member_id)? {
                set_ttl(times.lease_seconds)?;
            }
            return Ok(());
        }
        set_ttl(times.lease_seconds)?;
        std::thread::sleep(wait::poll_interval().saturating_mul(HYDRATION_POLLS));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::pool::new_member;

    fn restoring(percent: Option<u8>, stalled: bool) -> crate::provider::Sandbox {
        let mut sandbox = crate::provider::Sandbox::gone("bx_1");
        sandbox.state = SandboxState::Running;
        sandbox.hydration = crate::provider::Hydration::InProgress { percent };
        sandbox.restore_stalled = stalled;
        sandbox
    }

    #[test]
    fn a_restore_is_broken_only_after_a_whole_stall() {
        let limit = Duration::from_secs(600);
        let t0 = Instant::now();
        let at = |minutes: u64| t0 + Duration::from_secs(minutes * 60);
        // Progressing: never broken.
        let mut watch = StallWatch::default();
        for (minute, percent) in [(0, 10), (6, 20), (12, 30), (18, 40), (24, 50)] {
            assert_eq!(
                watch.look(&restoring(Some(percent), false), at(minute), limit),
                None
            );
        }
        // No progress for ten minutes: broken.
        let mut watch = StallWatch::default();
        assert_eq!(watch.look(&restoring(Some(40), false), at(0), limit), None);
        assert_eq!(watch.look(&restoring(Some(40), false), at(9), limit), None);
        let reason = watch
            .look(&restoring(Some(40), false), at(10), limit)
            .unwrap();
        assert!(reason.contains("no progress for 10 minutes"), "{reason}");
        // A reported stall that recovers starts over.
        let mut watch = StallWatch::default();
        assert_eq!(watch.look(&restoring(Some(5), true), at(0), limit), None);
        assert_eq!(watch.look(&restoring(Some(6), false), at(8), limit), None);
        assert_eq!(watch.look(&restoring(Some(6), true), at(9), limit), None);
        assert_eq!(watch.look(&restoring(Some(7), true), at(17), limit), None);
        assert!(
            watch
                .look(&restoring(Some(8), true), at(18), limit)
                .is_some()
        );
        // Unknown progress is not judged; a degraded machine is.
        let mut watch = StallWatch::default();
        assert_eq!(watch.look(&restoring(None, false), at(0), limit), None);
        assert_eq!(watch.look(&restoring(None, false), at(30), limit), None);
        let mut degraded = restoring(None, false);
        degraded.hydration = crate::provider::Hydration::Complete;
        degraded.degraded_since = Some("2026-11-15T04:43:00Z".into());
        assert_eq!(watch.look(&degraded, at(31), limit), None);
        let reason = watch.look(&degraded, at(41), limit).unwrap();
        assert!(reason.contains("could not reach"), "{reason}");
        let mut healthy = degraded.clone();
        healthy.degraded_since = None;
        assert_eq!(watch.look(&healthy, at(42), limit), None);
    }

    #[test]
    fn a_member_in_use_keeps_being_extended_until_its_user_lets_go() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let mut heartbeat = Heartbeat::start(Duration::from_millis(60), move || {
            move || recorded.lock().unwrap().push(("bx_test", 900))
        });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(calls.lock().unwrap().len(), 1, "the deadline moves at once");
        std::thread::sleep(Duration::from_millis(250));
        heartbeat.stop();
        let extended = calls.lock().unwrap().len();
        assert!(extended >= 3, "only {extended} extensions");
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| *call == ("bx_test", 900))
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(calls.lock().unwrap().len(), extended, "extended after stop");
    }

    fn scratch_store(label: &str) -> (Store, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("boatparty-lease-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.clone().into_os_string();
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        (Store::new(&paths), dir)
    }

    #[test]
    fn only_the_last_leaver_sets_the_idle_deadline() {
        let (store, dir) = scratch_store("last");
        let mut member = new_member("bp-app-1".into(), "key-1".into(), Some("/w".into()), 0);
        member.id = Some("bx_1".into());
        store
            .update(|state| {
                state.project_mut("app").members.push(member.clone());
                Ok(())
            })
            .unwrap();
        // Another live process (a child) holds a lease too.
        let mut other = Command::new("sleep").arg("30").spawn().unwrap();
        let other_pid = other.id();
        assert!(register(&store, "app", "/w", &member, LeaseKind::Run, "cargo test").unwrap());
        // A workspace the member does not belong to gets no lease.
        assert!(!register(&store, "app", "/v", &member, LeaseKind::Run, "x").unwrap());
        store
            .update(|state| {
                state.project_mut("app").members[0].leases.push(Lease {
                    pid: other_pid,
                    kind: LeaseKind::Serve,
                    label: "dev".into(),
                    started_at: 0,
                    workspace: Some("/w".into()),
                });
                Ok(())
            })
            .unwrap();

        let mut idle_set = 0;
        let last = finish(&store, "app", "bx_1", |_| {}, || idle_set += 1).unwrap();
        assert!(!last);
        assert_eq!(idle_set, 0, "set the idle deadline under a live lease");
        assert!(has_live_lease(&store, "app", "bx_1").unwrap());

        let _ = other.kill();
        let _ = other.wait();
        assert!(register(&store, "app", "/w", &member, LeaseKind::Run, "cargo test").unwrap());
        let last = finish(
            &store,
            "app",
            "bx_1",
            |member| member.last_run = Some(serde_json::json!({"exit_code": 0})),
            || idle_set += 1,
        )
        .unwrap();
        assert!(last);
        assert_eq!(idle_set, 1);
        let state = store.load().unwrap();
        let member = &state.projects["app"].members[0];
        assert!(member.leases.is_empty(), "dead and released leases stay");
        assert_eq!(member.last_run, Some(serde_json::json!({"exit_code": 0})));
        let _ = std::fs::remove_dir_all(dir);
    }
}
