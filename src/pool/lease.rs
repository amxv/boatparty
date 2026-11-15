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
use crate::account::config::Defaults;
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

/// The deadlines leases work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTimes {
    pub lease_seconds: u64,
    pub idle_seconds: u64,
    pub heartbeat: Duration,
}

impl LeaseTimes {
    pub fn from_defaults(defaults: &Defaults) -> Self {
        Self {
            lease_seconds: u64::from(defaults.lease_minutes) * 60,
            idle_seconds: u64::from(defaults.idle_minutes) * 60,
            heartbeat: Duration::from_secs(u64::from(defaults.heartbeat_minutes) * 60),
        }
    }
}

/// Add this process's lease to the member (matched by its create key),
/// dropping leases whose process is gone.
pub fn register(
    store: &Store,
    project: &str,
    member: &Member,
    kind: LeaseKind,
    label: &str,
) -> Result<(), AppError> {
    let pid = std::process::id();
    let label: String = label.chars().take(120).collect();
    store.update(|state| {
        let pool = state.project_mut(project);
        let slot = match pool.find_mut_by_key(&member.create_key) {
            Some(slot) => slot,
            None => {
                pool.members.push(member.clone());
                pool.members.last_mut().expect("just pushed")
            }
        };
        slot.leases
            .retain(|lease| lease.pid != pid && system::pid_alive(lease.pid));
        slot.leases.push(Lease {
            pid,
            kind,
            label,
            started_at: system::now_ms(),
        });
        slot.last_used_at = system::now_ms();
        Ok(())
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
    let times = LeaseTimes::from_defaults(&account.config.defaults);
    let provider = open_provider(paths)?;
    let set_ttl = |seconds: u64| {
        wait::retry(Duration::from_secs(60), wait::transient, || {
            provider.set_ttl(member_id, seconds)
        })
    };
    let deadline = Instant::now() + HYDRATION_WAIT;
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
        if sandbox.hydration.is_complete() || Instant::now() >= deadline {
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
        register(&store, "app", &member, LeaseKind::Run, "cargo test").unwrap();
        store
            .update(|state| {
                state.project_mut("app").members[0].leases.push(Lease {
                    pid: other_pid,
                    kind: LeaseKind::Serve,
                    label: "dev".into(),
                    started_at: 0,
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
        register(&store, "app", &member, LeaseKind::Run, "cargo test").unwrap();
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
