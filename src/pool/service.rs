//! Discover and claim sandbox ports using the workspace's live serve leases.
use crate::error::AppError;
use crate::paths::Paths;
use crate::state::{Lease, LeaseKind, Member, Service, Store};
use crate::system;
use std::fs::{File, TryLockError};
use std::path::PathBuf;

pub struct ServeGuard {
    _file: File,
    path: PathBuf,
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn guard_path(paths: &Paths, lease: &Lease) -> PathBuf {
    paths
        .state_dir()
        .join(format!("serve-{}-{}.lock", lease.pid, lease.started_at))
}

// The OS drops the lock even for SIGKILL. A zombie or a reused PID must
// never publish its predecessor's URL.
pub fn held(paths: &Paths, lease: &Lease) -> bool {
    if lease.service.is_none() {
        return system::pid_alive(lease.pid);
    }
    let path = guard_path(paths, lease);
    let Ok(file) = File::open(&path) else {
        return false;
    };
    match file.try_lock_shared() {
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => system::pid_alive(lease.pid),
        Ok(()) => {
            let _ = std::fs::remove_file(path);
            false
        }
    }
}

pub fn services<'a>(
    paths: &'a Paths,
    member: &'a Member,
) -> impl Iterator<Item = (&'a Lease, &'a Service)> {
    member.leases.iter().filter_map(move |lease| {
        let service = lease.service.as_ref()?;
        (lease.kind == LeaseKind::Serve && held(paths, lease)).then_some((lease, service))
    })
}

pub fn check(paths: &Paths, member: &Member, ports: &[u16]) -> Result<(), AppError> {
    for (lease, service) in services(paths, member) {
        if lease.pid == std::process::id() {
            continue;
        }
        if let Some(port) = ports.iter().find(|p| service.ports.contains(p)) {
            let at = service
                .forwards
                .iter()
                .find(|f| f.remote_port == *port && service.started)
                .map_or(String::new(), |f| format!(" at {}", f.url));
            let age = system::now_ms().saturating_sub(lease.started_at) / 1000;
            return Err(AppError::new(
                "already_serving",
                format!(
                    "port {port} is already served for this workspace{at} (pid {}, started {age}s ago); use it or stop that serve",
                    lease.pid
                ),
            ));
        }
    }
    Ok(())
}

pub fn claim(
    paths: &Paths,
    project: &str,
    key: &str,
    service: Service,
) -> Result<ServeGuard, AppError> {
    Store::new(paths).update(|state| {
        let member = state
            .project_mut(project)
            .find_mut_by_key(key)
            .ok_or_else(missing)?;
        check(paths, member, &service.ports)?;
        let lease = mine(member)?;
        let path = guard_path(paths, lease);
        let file = system::private_file()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| AppError::io("opening serve lock", e))?;
        file.try_lock()
            .map_err(|e| AppError::io("claiming serve lock", e.into()))?;
        lease.service = Some(service);
        Ok(ServeGuard { _file: file, path })
    })
}

pub fn started(paths: &Paths, project: &str, key: &str) -> Result<(), AppError> {
    Store::new(paths).update(|state| {
        let member = state
            .project_mut(project)
            .find_mut_by_key(key)
            .ok_or_else(missing)?;
        mine(member)?.service.as_mut().ok_or_else(missing)?.started = true;
        Ok(())
    })
}

fn mine(member: &mut Member) -> Result<&mut Lease, AppError> {
    member
        .leases
        .iter_mut()
        .find(|l| l.pid == std::process::id() && l.kind == LeaseKind::Serve)
        .ok_or_else(missing)
}

fn missing() -> AppError {
    AppError::new("no_member", "the server lost its member lease")
}
