//! The state transitions around the acquisition policy: acquiring a member
//! for a workspace (with the lease taken in the same locked write),
//! releasing a workspace's affinity and pinning a member to a workspace.
//!
//! Each transition runs inside one `Store::update`, so the invariants hold
//! after every write:
//! - a member has at most one affinity;
//! - a workspace has at most one affinity member;
//! - every live lease on a member comes from its affinity workspace;
//! - affinity changes only while the member has no live lease.

use std::collections::{BTreeMap, BTreeSet};

use super::new_member;
use super::policy::{self, Candidate, Decision, Limits, Reassign, Running, WaitFor};
use crate::error::AppError;
use crate::state::{Lease, LeaseKind, Member, ProjectState, Role, State};
use crate::system;

/// A lease to take together with the member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRequest {
    pub kind: LeaseKind,
    pub label: String,
}

impl LeaseRequest {
    pub fn new(kind: LeaseKind, label: &str) -> Self {
        Self {
            kind,
            label: label.chars().take(120).collect(),
        }
    }
}

/// One acquisition attempt.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub project: &'a str,
    pub workspace: &'a str,
    pub limits: Limits,
    /// `max_running` of the project and of the account.
    pub max_running: RunningCaps,
    pub lease: Option<&'a LeaseRequest>,
    /// The lease holder.
    pub pid: u32,
    /// The create key for a new member, should one be created.
    pub new_key: &'a str,
    pub now: u64,
}

/// The limits on running machines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningCaps {
    pub project: Option<u32>,
    pub account: Option<u32>,
}

impl RunningCaps {
    pub fn any(&self) -> bool {
        self.project.is_some() || self.account.is_some()
    }
}

/// How many machines run, as counted for `max_running`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningCount {
    pub project: Option<u64>,
    pub account: Option<u64>,
}

/// How the member was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    Affinity,
    Free,
    Created,
    Reassigned { from: String, why: Reassign },
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Acquired {
        member: Box<Member>,
        how: How,
    },
    /// Nothing is free; `members` is the pool's size.
    Wait {
        members: usize,
        why: WaitFor,
        running: RunningCount,
    },
}

/// What the caller knows about the world outside the state file.
pub struct Facts<'a> {
    /// Provider id to "is running", from one listing.
    pub running: &'a BTreeMap<String, bool>,
    /// Whether a workspace directory exists.
    pub exists: &'a dyn Fn(&str) -> bool,
    /// The project's sandboxes that have a machine (running or starting),
    /// when they were listed.
    pub active: Option<&'a BTreeSet<String>>,
    /// Sandboxes with a machine across the account, when fetched.
    pub account_running: Option<u64>,
}

impl<'a> Facts<'a> {
    /// Facts with no running information (nothing listed or fetched).
    pub fn local(running: &'a BTreeMap<String, bool>, exists: &'a dyn Fn(&str) -> bool) -> Self {
        Self {
            running,
            exists,
            active: None,
            account_running: None,
        }
    }
}

/// Machines counted for `max_running`: those listed with a machine, plus
/// members another live process is creating or starting right now.
pub(super) fn count_running(pool: &ProjectState, facts: &Facts<'_>) -> RunningCount {
    let starting = pool
        .members
        .iter()
        .filter(|member| {
            member.bring_up_claimed_by_other()
                && member
                    .id
                    .as_ref()
                    .is_none_or(|id| facts.active.is_none_or(|active| !active.contains(id)))
        })
        .count() as u64;
    RunningCount {
        project: facts.active.map(|active| active.len() as u64 + starting),
        account: facts.account_running.map(|running| running + starting),
    }
}

/// Whether one more machine may run. Unknown counts never block.
pub(super) fn has_room(caps: RunningCaps, count: RunningCount) -> bool {
    let fits = |cap: Option<u32>, count: Option<u64>| match (cap, count) {
        (Some(cap), Some(count)) => count < u64::from(cap),
        _ => true,
    };
    fits(caps.project, count.project) && fits(caps.account, count.account)
}

fn running_of(member: &Member, running: &BTreeMap<String, bool>) -> Running {
    match member.id.as_ref().and_then(|id| running.get(id)) {
        Some(true) => Running::Yes,
        Some(false) => Running::No,
        None => Running::Unknown,
    }
}

/// Decide and apply one acquisition on the locked state.
pub fn acquire(state: &mut State, request: &Request<'_>, facts: &Facts<'_>) -> Outcome {
    let pool = state.project_mut(request.project);
    let running = count_running(pool, facts);
    let room = has_room(request.max_running, running);
    let decision = {
        let candidates: Vec<Candidate<'_>> = pool
            .members
            .iter()
            .filter(|member| member.role == Role::Pool)
            .map(|member| Candidate {
                key: &member.create_key,
                affinity: member.affinity.as_deref(),
                busy: member.busy(),
                running: running_of(member, facts.running),
                workspace_exists: member
                    .affinity
                    .as_deref()
                    .is_none_or(|workspace| (facts.exists)(workspace)),
                last_used_at: member.last_used_at,
            })
            .collect();
        policy::decide(
            &candidates,
            request.workspace,
            Limits {
                max_sandboxes: request.limits.max_sandboxes.saturating_sub(u32::from(
                    pool.members.iter().any(|member| member.role == Role::Gate),
                )),
                ..request.limits
            },
            room,
            request.now,
        )
    };
    let (key, how) = match decision {
        Decision::Wait(why) => {
            return Outcome::Wait {
                members: pool
                    .members
                    .iter()
                    .filter(|member| member.role == Role::Pool)
                    .count(),
                why,
                running,
            };
        }
        Decision::Use { key } => (key, How::Affinity),
        Decision::Take { key } => (key, How::Free),
        Decision::Reassign { key, from, why } => (key, How::Reassigned { from, why }),
        Decision::Create => {
            let name = pool.next_member_name(request.project);
            let member = new_member(
                name,
                request.new_key.to_string(),
                Some(request.workspace.to_string()),
                request.now,
            );
            pool.members.push(member);
            (request.new_key.to_string(), How::Created)
        }
    };
    // Under max_running the workspace may move off its own (stopped, unused)
    // member to a running one; the old member keeps no affinity.
    for member in &mut pool.members {
        if member.role == Role::Pool
            && member.create_key != key
            && member.affinity.as_deref() == Some(request.workspace)
        {
            member.affinity = None;
            member.leases.retain(|lease| system::pid_alive(lease.pid));
        }
    }
    if request.lease.is_some() {
        // One process holds at most one lease in a project.
        for member in &mut pool.members {
            member.leases.retain(|held| held.pid != request.pid);
        }
    }
    let member = pool
        .find_mut_by_key(&key)
        .expect("the policy picks an existing record");
    member.affinity = Some(request.workspace.to_string());
    member.leases.retain(|lease| system::pid_alive(lease.pid));
    if let Some(lease) = request.lease
        && !member.bring_up_claimed_by_other()
    {
        member.leases.push(Lease {
            pid: request.pid,
            kind: lease.kind,
            label: lease.label.clone(),
            started_at: request.now,
            workspace: Some(request.workspace.to_string()),
            service: None,
        });
    }
    member.last_used_at = request.now;
    Outcome::Acquired {
        member: Box::new(member.clone()),
        how,
    }
}

fn in_use(member: &Member, action: &str) -> AppError {
    let holders: Vec<String> = member
        .live_leases()
        .map(|lease| format!("pid {} ({})", lease.pid, lease.label))
        .chain(
            member
                .bringing_up
                .as_ref()
                .filter(|_| member.bring_up_claimed_by_other())
                .map(|claim| format!("pid {} (starting it)", claim.pid)),
        )
        .collect();
    AppError::new(
        "member_in_use",
        format!(
            "cannot {action} {}: it is in use by {}",
            member.name,
            holders.join(", ")
        ),
    )
    .with_hint("wait for those commands to finish")
}

/// Drop this workspace's affinity. Refused while the member is in use.
/// Returns the member that was released, if the workspace had one.
pub fn release(
    state: &mut State,
    project: &str,
    workspace: &str,
) -> Result<Option<Member>, AppError> {
    let pool = state.project_mut(project);
    let Some(member) = pool
        .members
        .iter_mut()
        .find(|member| member.role == Role::Pool && member.affinity.as_deref() == Some(workspace))
    else {
        return Ok(None);
    };
    if member.busy() {
        return Err(in_use(member, "release"));
    }
    member.affinity = None;
    member.leases.clear();
    Ok(Some(member.clone()))
}

/// What `assign` changed.
#[derive(Debug, Clone)]
pub struct Assigned {
    pub member: Member,
    /// The workspace's previous member, which lost its affinity.
    pub previous: Option<Member>,
    /// The member already belonged to this workspace.
    pub unchanged: bool,
}

/// Pin `key` (a name or id) to this workspace. The member must be free: no
/// live lease and no affinity with an existing workspace. A previous member
/// of this workspace loses its affinity, which needs it to be unused too.
pub fn assign(
    state: &mut State,
    project: &str,
    workspace: &str,
    key: &str,
    exists: &dyn Fn(&str) -> bool,
    now: u64,
) -> Result<Assigned, AppError> {
    let pool = state.project_mut(project);
    let Some(target) = pool.find(key).cloned() else {
        return Err(AppError::new(
            "no_member",
            format!("project {project} has no sandbox named {key}"),
        )
        .with_hint("`boatparty pool` lists them"));
    };
    if target.role != Role::Pool {
        return Err(AppError::new(
            "no_member",
            format!("{} is not a pool member", target.name),
        ));
    }
    if target.affinity.as_deref() == Some(workspace) {
        return Ok(Assigned {
            member: target,
            previous: None,
            unchanged: true,
        });
    }
    if target.busy() {
        return Err(in_use(&target, "assign"));
    }
    if let Some(other) = target.affinity.as_deref()
        && exists(other)
    {
        return Err(AppError::new(
            "member_assigned",
            format!("{} belongs to the workspace {other}", target.name),
        )
        .with_hint(format!(
            "run `boatparty release` in {other} first, or pick a free member from `boatparty pool`"
        )));
    }
    let previous = match own_member_mut(pool, workspace) {
        Some(own) if own.busy() => return Err(in_use(own, "move this workspace away from")),
        Some(own) => {
            own.affinity = None;
            own.leases.clear();
            Some(own.clone())
        }
        None => None,
    };
    let slot = pool
        .find_mut_by_key(&target.create_key)
        .expect("found above");
    slot.affinity = Some(workspace.to_string());
    slot.leases.clear();
    slot.last_used_at = now;
    Ok(Assigned {
        member: slot.clone(),
        previous,
        unchanged: false,
    })
}

fn own_member_mut<'a>(pool: &'a mut ProjectState, workspace: &str) -> Option<&'a mut Member> {
    pool.members
        .iter_mut()
        .find(|member| member.role == Role::Pool && member.affinity.as_deref() == Some(workspace))
}

/// The invariants, checked against live leases. Used by tests.
pub fn check_invariants(pool: &ProjectState, max_sandboxes: Option<usize>) -> Result<(), String> {
    let members: Vec<&Member> = pool
        .members
        .iter()
        .filter(|member| member.role != Role::Builder)
        .collect();
    if let Some(max) = max_sandboxes
        && members.len() > max
    {
        return Err(format!("{} members, cap {max}", members.len()));
    }
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for member in &members {
        if let Some(workspace) = member.affinity.as_deref()
            && let Some(other) = seen.insert(workspace, &member.name)
        {
            return Err(format!(
                "workspace {workspace} has two members: {other} and {}",
                member.name
            ));
        }
        for lease in member.live_leases() {
            if lease.workspace.is_some() && lease.workspace != member.affinity {
                return Err(format!(
                    "{} has a live lease from {:?} but belongs to {:?}",
                    member.name, lease.workspace, member.affinity
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Child;
    use std::sync::Mutex;

    use super::*;
    use crate::paths::Paths;
    use crate::state::Store;

    fn scratch(label: &str) -> (Store, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("boatparty-acquire-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = dir.join("home").into_os_string();
        let paths =
            Paths::resolve(|var| (var == crate::paths::HOME_ENV).then(|| home.clone())).unwrap();
        (Store::new(&paths), dir)
    }

    fn sleeper() -> Child {
        crate::system::test_child()
    }

    fn request<'a>(
        workspace: &'a str,
        lease: Option<&'a LeaseRequest>,
        pid: u32,
        key: &'a str,
        max: u32,
        steal_after_ms: u64,
    ) -> Request<'a> {
        Request {
            project: "app",
            workspace,
            limits: Limits {
                max_sandboxes: max,
                steal_after_ms,
            },
            max_running: RunningCaps::default(),
            lease,
            pid,
            new_key: key,
            now: system::now_ms(),
        }
    }

    #[test]
    fn concurrent_workspaces_never_break_the_invariants() {
        const WORKSPACES: usize = 5;
        const CAP: u32 = 2;
        const ROUNDS: usize = 6;
        let (store, dir) = scratch("concurrent");
        let workspaces: Vec<String> = (0..WORKSPACES)
            .map(|n| {
                let path = dir.join(format!("ws{n}"));
                std::fs::create_dir_all(&path).unwrap();
                path.display().to_string()
            })
            .collect();
        // Who holds each member right now, by create key: (workspace, holders).
        let holders: Mutex<HashMap<String, (String, usize)>> = Mutex::new(HashMap::new());
        let violations: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let running = BTreeMap::new();
        let exists = |path: &str| std::path::Path::new(path).is_dir();
        std::thread::scope(|scope| {
            for (index, workspace) in workspaces.iter().enumerate() {
                // Two processes per workspace: several may hold leases at once.
                for worker in 0..2 {
                    let store = store.clone();
                    let holders = &holders;
                    let violations = &violations;
                    let running = &running;
                    let exists = &exists;
                    scope.spawn(move || {
                        let lease = LeaseRequest::new(LeaseKind::Run, "test");
                        for round in 0..ROUNDS {
                            let mut child = sleeper();
                            let pid = child.id();
                            let key = format!("k-{index}-{worker}-{round}");
                            let member = loop {
                                let outcome = store
                                    .update(|state| {
                                        let outcome = acquire(
                                            state,
                                            &request(workspace, Some(&lease), pid, &key, CAP, 0),
                                            &Facts::local(running, exists),
                                        );
                                        if let Err(error) = check_invariants(
                                            &state.projects["app"],
                                            Some(CAP as usize),
                                        ) {
                                            violations.lock().unwrap().push(error);
                                        }
                                        Ok(outcome)
                                    })
                                    .unwrap();
                                match outcome {
                                    Outcome::Acquired { member, .. } => break member,
                                    Outcome::Wait { .. } => {
                                        std::thread::sleep(std::time::Duration::from_millis(3));
                                    }
                                }
                            };
                            {
                                let mut held = holders.lock().unwrap();
                                let entry = held
                                    .entry(member.create_key.clone())
                                    .or_insert_with(|| (workspace.clone(), 0));
                                if entry.1 > 0 && entry.0 != *workspace {
                                    violations.lock().unwrap().push(format!(
                                        "{} reassigned from {} to {workspace} under a live lease",
                                        member.name, entry.0
                                    ));
                                }
                                entry.0 = workspace.clone();
                                entry.1 += 1;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            {
                                let mut held = holders.lock().unwrap();
                                let entry = held.get_mut(&member.create_key).unwrap();
                                entry.1 -= 1;
                            }
                            let _ = child.kill();
                            let _ = child.wait();
                            store
                                .update(|state| {
                                    for member in &mut state.project_mut("app").members {
                                        member.leases.retain(|lease| lease.pid != pid);
                                    }
                                    // Now and then a workspace lets go explicitly.
                                    if round % 3 == 2 {
                                        let _ = release(state, "app", workspace);
                                    }
                                    if let Err(error) =
                                        check_invariants(&state.projects["app"], Some(CAP as usize))
                                    {
                                        violations.lock().unwrap().push(error);
                                    }
                                    Ok(())
                                })
                                .unwrap();
                        }
                    });
                }
            }
        });
        let violations = violations.into_inner().unwrap();
        assert!(violations.is_empty(), "{violations:#?}");
        let state = store.load().unwrap();
        assert!(state.projects["app"].members.len() <= CAP as usize);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn member(name: &str, affinity: Option<&str>) -> Member {
        let mut member = new_member(
            name.into(),
            format!("key-{name}"),
            affinity.map(Into::into),
            0,
        );
        member.id = Some(format!("bx_{name}"));
        member
    }

    fn state_with(members: Vec<Member>) -> State {
        let mut state = State::default();
        state.project_mut("app").members = members;
        state
    }

    #[test]
    fn release_drops_affinity_only_without_a_live_lease() {
        let mut state = state_with(vec![member("a", Some("/w"))]);
        let released = release(&mut state, "app", "/w").unwrap().unwrap();
        assert_eq!(released.affinity, None);
        assert!(release(&mut state, "app", "/w").unwrap().is_none());

        let mut child = sleeper();
        let mut leased = member("b", Some("/w"));
        leased.leases.push(Lease {
            pid: child.id(),
            kind: LeaseKind::Run,
            label: "cargo test".into(),
            started_at: 0,
            workspace: Some("/w".into()),
            service: None,
        });
        let mut state = state_with(vec![leased]);
        let error = release(&mut state, "app", "/w").unwrap_err();
        assert_eq!(error.code(), "member_in_use");
        assert_eq!(
            state.projects["app"].members[0].affinity.as_deref(),
            Some("/w")
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn assign_takes_free_members_and_moves_the_previous_one_out() {
        let exists = |path: &str| path != "/gone";
        let mut state = state_with(vec![
            member("own", Some("/w")),
            member("free", None),
            member("orphan", Some("/gone")),
            member("taken", Some("/other")),
        ]);
        let assigned = assign(&mut state, "app", "/w", "free", &exists, 5).unwrap();
        assert_eq!(assigned.member.affinity.as_deref(), Some("/w"));
        assert_eq!(assigned.previous.unwrap().name, "own");
        check_invariants(&state.projects["app"], None).unwrap();

        // A member of a workspace that is gone is free too; assigning it by id works.
        let assigned = assign(&mut state, "app", "/w", "bx_orphan", &exists, 6).unwrap();
        assert_eq!(assigned.member.name, "orphan");
        check_invariants(&state.projects["app"], None).unwrap();

        // Another existing workspace's member is refused, and so is an unknown one.
        let error = assign(&mut state, "app", "/w", "taken", &exists, 7).unwrap_err();
        assert_eq!(error.code(), "member_assigned");
        let error = assign(&mut state, "app", "/w", "nope", &exists, 7).unwrap_err();
        assert_eq!(error.code(), "no_member");
        // Assigning the current member again changes nothing.
        let again = assign(&mut state, "app", "/w", "orphan", &exists, 8).unwrap();
        assert!(again.unchanged);

        // A leased member is refused.
        let mut child = sleeper();
        let mut leased = member("leased", None);
        leased.leases.push(Lease {
            pid: child.id(),
            kind: LeaseKind::Session,
            label: String::new(),
            started_at: 0,
            workspace: None,
            service: None,
        });
        state.project_mut("app").members.push(leased);
        let error = assign(&mut state, "app", "/w", "leased", &exists, 9).unwrap_err();
        assert_eq!(error.code(), "member_in_use");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn acquisition_takes_the_lease_in_the_same_write() {
        let mut state = state_with(vec![member("a", None)]);
        let lease = LeaseRequest::new(LeaseKind::Run, "cargo test");
        let running = BTreeMap::new();
        let exists = |_: &str| true;
        let pid = std::process::id();
        let outcome = acquire(
            &mut state,
            &request("/w", Some(&lease), pid, "new", 1, 0),
            &Facts::local(&running, &exists),
        );
        let Outcome::Acquired { member, how } = outcome else {
            panic!("expected a member");
        };
        assert_eq!(how, How::Free);
        assert_eq!(member.affinity.as_deref(), Some("/w"));
        assert_eq!(member.leases.len(), 1);
        assert_eq!(member.leases[0].workspace.as_deref(), Some("/w"));
        // A second workspace has to wait: the cap is 1 and the member is leased.
        let outcome = acquire(
            &mut state,
            &request("/v", None, pid + 1, "other", 1, 0),
            &Facts::local(&running, &exists),
        );
        assert!(matches!(
            outcome,
            Outcome::Wait {
                members: 1,
                why: WaitFor::Member,
                ..
            }
        ));
    }

    #[test]
    fn at_max_running_a_workspace_moves_to_a_running_member_and_lets_its_own_go() {
        let mut state = state_with(vec![member("own", Some("/w")), member("other", Some("/v"))]);
        let running = BTreeMap::from([
            ("bx_own".to_string(), false),
            ("bx_other".to_string(), true),
        ]);
        let active = BTreeSet::from(["bx_other".to_string()]);
        let exists = |_: &str| true;
        let mut capped = request("/w", None, std::process::id(), "new", 4, 0);
        capped.max_running = RunningCaps {
            project: Some(1),
            account: None,
        };
        let facts = Facts {
            running: &running,
            exists: &exists,
            active: Some(&active),
            account_running: None,
        };
        let Outcome::Acquired { member: got, how } = acquire(&mut state, &capped, &facts) else {
            panic!("expected a member");
        };
        assert_eq!(got.name, "other");
        assert_eq!(
            how,
            How::Reassigned {
                from: "/v".into(),
                why: Reassign::RunningCap
            }
        );
        let pool = &state.projects["app"];
        assert_eq!(pool.find("own").unwrap().affinity, None);
        check_invariants(pool, None).unwrap();

        // The account's cap counts too; with room the own member is used.
        let mut state = state_with(vec![member("own", Some("/w"))]);
        capped.max_running = RunningCaps {
            project: None,
            account: Some(3),
        };
        let facts = Facts {
            running: &running,
            exists: &exists,
            active: Some(&active),
            account_running: Some(3),
        };
        assert!(matches!(
            acquire(&mut state, &capped, &facts),
            Outcome::Wait {
                why: WaitFor::Running,
                running: RunningCount {
                    account: Some(3),
                    ..
                },
                ..
            }
        ));
        let facts = Facts {
            account_running: Some(2),
            ..facts
        };
        assert!(matches!(
            acquire(&mut state, &capped, &facts),
            Outcome::Acquired {
                how: How::Affinity,
                ..
            }
        ));
    }
}
