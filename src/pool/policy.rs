//! The acquisition policy: which member serves a workspace. Pure: it sees
//! facts gathered by the caller and returns a decision; it never touches
//! the state file, a process, the file system or the provider.
//!
//! Order: (1) the workspace's affinity member; (2) a member with no
//! affinity, preferring a running one; (3) a new member while the pool is
//! below its cap; (4) an idle member of another workspace, preferring one
//! whose workspace no longer exists, then the least recently used one idle
//! longer than `steal_after`, preferring running over stopped in both;
//! (5) wait.
//!
//! When `max_running` (the project's or the account's) leaves no room for
//! another running machine, a choice that would start one is replaced by a
//! running member nobody is using: a free one, else another workspace's
//! (its workspace gone first, then the least recently used, at any idle
//! age). With none, the caller waits for room.

use std::cmp::Reverse;

/// Whether a member's machine runs, as last reported by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Running {
    Yes,
    Unknown,
    No,
}

/// One pool member as the policy sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// The member's create key, which identifies its record.
    pub key: &'a str,
    pub affinity: Option<&'a str>,
    /// A live lease or a live bring-up by another process.
    pub busy: bool,
    pub running: Running,
    /// Whether the affinity workspace's directory exists (true without one).
    pub workspace_exists: bool,
    pub last_used_at: u64,
}

/// The limits the policy enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_sandboxes: u32,
    pub steal_after_ms: u64,
}

/// Why a member changes workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reassign {
    /// Its workspace's directory no longer exists.
    WorkspaceGone,
    /// Nobody used it for longer than `steal_after`.
    Idle,
    /// `max_running` is reached and it runs with nobody using it.
    RunningCap,
}

/// What a wait is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitFor {
    /// Every member is in use at `max_sandboxes`.
    Member,
    /// `max_running` leaves no room to start one.
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Step 1: the workspace's own member.
    Use { key: String },
    /// Step 2: a member nobody has affinity with.
    Take { key: String },
    /// Step 3: a new member.
    Create,
    /// Step 4: another workspace's idle member.
    Reassign {
        key: String,
        from: String,
        why: Reassign,
    },
    /// Step 5: nothing is free.
    Wait(WaitFor),
}

/// Decide which member serves `workspace`. `members` are the project's pool
/// members (builders excluded); `room` says whether `max_running` allows
/// one more running machine; `now` is in milliseconds.
pub fn decide(
    members: &[Candidate<'_>],
    workspace: &str,
    limits: Limits,
    room: bool,
    now: u64,
) -> Decision {
    let decision = decide_unbounded(members, workspace, limits, now);
    if room || !starts_a_machine(members, &decision) {
        return decision;
    }
    // No room for another machine: a running member nobody uses instead.
    let idle_running = |member: &&Candidate<'_>| member.running == Running::Yes && !member.busy;
    if let Some(free) = members
        .iter()
        .filter(|member| member.affinity.is_none())
        .filter(idle_running)
        .max_by_key(|member| member.last_used_at)
    {
        return Decision::Take {
            key: free.key.to_string(),
        };
    }
    if let Some(other) = members
        .iter()
        .filter(|member| {
            member
                .affinity
                .is_some_and(|affinity| affinity != workspace)
        })
        .filter(idle_running)
        .min_by_key(|member| (member.workspace_exists, member.last_used_at))
    {
        return Decision::Reassign {
            key: other.key.to_string(),
            from: other.affinity.unwrap_or_default().to_string(),
            why: if other.workspace_exists {
                Reassign::RunningCap
            } else {
                Reassign::WorkspaceGone
            },
        };
    }
    Decision::Wait(WaitFor::Running)
}

/// Whether carrying out `decision` starts a machine: a new member, or a
/// member that does not run and that nobody else is using or starting.
fn starts_a_machine(members: &[Candidate<'_>], decision: &Decision) -> bool {
    let key = match decision {
        Decision::Create => return true,
        Decision::Wait(_) => return false,
        Decision::Use { key } | Decision::Take { key } | Decision::Reassign { key, .. } => key,
    };
    members
        .iter()
        .find(|member| member.key == key)
        .is_some_and(|member| member.running != Running::Yes && !member.busy)
}

/// The acquisition order without `max_running`.
fn decide_unbounded(
    members: &[Candidate<'_>],
    workspace: &str,
    limits: Limits,
    now: u64,
) -> Decision {
    if let Some(own) = members
        .iter()
        .find(|member| member.affinity == Some(workspace))
    {
        return Decision::Use {
            key: own.key.to_string(),
        };
    }

    if let Some(free) = members
        .iter()
        .filter(|member| member.affinity.is_none() && !member.busy)
        .min_by_key(|member| (member.running, Reverse(member.last_used_at)))
    {
        return Decision::Take {
            key: free.key.to_string(),
        };
    }

    if members.len() < limits.max_sandboxes as usize {
        return Decision::Create;
    }

    let others = || {
        members
            .iter()
            .filter(|member| member.affinity.is_some() && !member.busy)
    };
    let pick = |why: Reassign, member: &Candidate<'_>| Decision::Reassign {
        key: member.key.to_string(),
        from: member.affinity.unwrap_or_default().to_string(),
        why,
    };
    if let Some(orphan) = others()
        .filter(|member| !member.workspace_exists)
        .min_by_key(|member| (member.running, member.last_used_at))
    {
        return pick(Reassign::WorkspaceGone, orphan);
    }
    if let Some(idle) = others()
        .filter(|member| now.saturating_sub(member.last_used_at) > limits.steal_after_ms)
        .min_by_key(|member| (member.running, member.last_used_at))
    {
        return pick(Reassign::Idle, idle);
    }
    Decision::Wait(WaitFor::Member)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;
    const NOW: u64 = 1_000 * MIN;
    const LIMITS: Limits = Limits {
        max_sandboxes: 3,
        steal_after_ms: 30 * MIN,
    };

    /// A member in the table: key, affinity, busy, running, workspace
    /// exists, minutes since last use.
    type Row = (&'static str, Option<&'static str>, bool, Running, bool, u64);

    fn candidates(rows: &[Row]) -> Vec<Candidate<'static>> {
        rows.iter()
            .map(|&(key, affinity, busy, running, exists, idle)| Candidate {
                key,
                affinity,
                busy,
                running,
                workspace_exists: exists,
                last_used_at: NOW - idle * MIN,
            })
            .collect()
    }

    fn take(key: &str) -> Decision {
        Decision::Take { key: key.into() }
    }

    fn reassign(key: &str, from: &str, why: Reassign) -> Decision {
        Decision::Reassign {
            key: key.into(),
            from: from.into(),
            why,
        }
    }

    use Running::{No, Unknown, Yes};

    #[test]
    fn the_acquisition_order_and_every_preference() {
        let use_ = |key: &str| Decision::Use { key: key.into() };
        let cases: Vec<(&str, Vec<Row>, u32, Decision)> = vec![
            // Step 1: the affinity member, even when busy, stopped or idle,
            // and even when free members exist.
            (
                "own member first",
                vec![
                    ("a", None, false, Yes, true, 0),
                    ("b", Some("/w"), true, No, true, 90),
                ],
                3,
                use_("b"),
            ),
            (
                "own member while the pool is empty otherwise",
                vec![("b", Some("/w"), false, Unknown, true, 0)],
                1,
                use_("b"),
            ),
            // Step 2: a member without affinity, running preferred.
            (
                "free member, running preferred",
                vec![
                    ("stopped", None, false, No, true, 0),
                    ("running", None, false, Yes, true, 50),
                    ("unknown", None, false, Unknown, true, 0),
                ],
                3,
                take("running"),
            ),
            (
                "free members of equal state: most recently used",
                vec![
                    ("old", None, false, No, true, 50),
                    ("new", None, false, No, true, 5),
                ],
                3,
                take("new"),
            ),
            (
                "a free member that is busy is not taken",
                vec![("busy", None, true, Yes, true, 0)],
                1,
                Decision::Wait(WaitFor::Member),
            ),
            // Step 3: create below the cap.
            ("empty pool", vec![], 3, Decision::Create),
            (
                "below the cap, even with idle members elsewhere",
                vec![("x", Some("/x"), false, Yes, false, 90)],
                3,
                Decision::Create,
            ),
            // Step 4a: a member whose workspace is gone, before an idle one.
            (
                "workspace gone beats idle",
                vec![
                    ("idle", Some("/i"), false, Yes, true, 90),
                    ("gone", Some("/g"), false, No, false, 1),
                ],
                2,
                reassign("gone", "/g", Reassign::WorkspaceGone),
            ),
            (
                "workspace gone: running preferred",
                vec![
                    ("g-stopped", Some("/g1"), false, No, false, 90),
                    ("g-running", Some("/g2"), false, Yes, false, 1),
                ],
                2,
                reassign("g-running", "/g2", Reassign::WorkspaceGone),
            ),
            (
                "workspace gone: least recently used among equals",
                vec![
                    ("g-new", Some("/g1"), false, No, false, 1),
                    ("g-old", Some("/g2"), false, No, false, 9),
                ],
                2,
                reassign("g-old", "/g2", Reassign::WorkspaceGone),
            ),
            (
                "workspace gone but leased is not taken",
                vec![("g", Some("/g"), true, Yes, false, 90)],
                1,
                Decision::Wait(WaitFor::Member),
            ),
            // Step 4b: idle longer than steal_after.
            (
                "idle: running preferred over stopped",
                vec![
                    ("i-stopped", Some("/a"), false, No, true, 300),
                    ("i-running", Some("/b"), false, Yes, true, 40),
                ],
                2,
                reassign("i-running", "/b", Reassign::Idle),
            ),
            (
                "idle: least recently used among equals",
                vec![
                    ("i-40", Some("/a"), false, Yes, true, 40),
                    ("i-90", Some("/b"), false, Yes, true, 90),
                ],
                2,
                reassign("i-90", "/b", Reassign::Idle),
            ),
            (
                "idle: not long enough is not taken",
                vec![("recent", Some("/a"), false, Yes, true, 29)],
                1,
                Decision::Wait(WaitFor::Member),
            ),
            (
                "idle: a busy member is never taken",
                vec![
                    ("busy", Some("/a"), true, Yes, true, 900),
                    ("ok", Some("/b"), false, No, true, 31),
                ],
                2,
                reassign("ok", "/b", Reassign::Idle),
            ),
            // Step 5: wait.
            (
                "everything busy at the cap",
                vec![
                    ("a", Some("/a"), true, Yes, true, 0),
                    ("b", Some("/b"), true, Yes, true, 0),
                ],
                2,
                Decision::Wait(WaitFor::Member),
            ),
            (
                "over the cap (lowered later) and nothing free",
                vec![
                    ("a", Some("/a"), false, Yes, true, 1),
                    ("b", Some("/b"), false, Yes, true, 1),
                ],
                1,
                Decision::Wait(WaitFor::Member),
            ),
        ];
        for (name, rows, max, expected) in cases {
            let members = candidates(&rows);
            let limits = Limits {
                max_sandboxes: max,
                ..LIMITS
            };
            assert_eq!(
                decide(&members, "/w", limits, true, NOW),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn max_running_steers_toward_running_members_nobody_uses() {
        let use_ = |key: &str| Decision::Use { key: key.into() };
        let wait = Decision::Wait(WaitFor::Running);
        let cases: Vec<(&str, Vec<Row>, u32, Decision)> = vec![
            // Nothing would start: the cap changes nothing.
            (
                "own member already running",
                vec![
                    ("own", Some("/w"), false, Yes, true, 5),
                    ("free", None, false, Yes, true, 0),
                ],
                3,
                use_("own"),
            ),
            (
                "own member busy (another process is starting it)",
                vec![("own", Some("/w"), true, No, true, 5)],
                3,
                use_("own"),
            ),
            (
                "a running free member is taken as without the cap",
                vec![("free", None, false, Yes, true, 0)],
                3,
                take("free"),
            ),
            // A start would be needed: prefer a running member nobody uses.
            (
                "own member stopped: a running free member instead",
                vec![
                    ("own", Some("/w"), false, No, true, 5),
                    ("free", None, false, Yes, true, 0),
                ],
                3,
                take("free"),
            ),
            (
                "own member stopped: another workspace's running idle member, any idle age",
                vec![
                    ("own", Some("/w"), false, No, true, 5),
                    ("theirs", Some("/b"), false, Yes, true, 1),
                ],
                3,
                reassign("theirs", "/b", Reassign::RunningCap),
            ),
            (
                "below max_sandboxes: reassign rather than create",
                vec![
                    ("stopped-free", None, false, No, true, 0),
                    ("recent", Some("/b"), false, Yes, true, 1),
                    ("older", Some("/c"), false, Yes, true, 9),
                ],
                5,
                reassign("older", "/c", Reassign::RunningCap),
            ),
            (
                "a running member whose workspace is gone goes first",
                vec![
                    ("older", Some("/c"), false, Yes, true, 9),
                    ("orphan", Some("/g"), false, Yes, false, 1),
                ],
                5,
                reassign("orphan", "/g", Reassign::WorkspaceGone),
            ),
            (
                "busy running members are never taken: wait for room",
                vec![
                    ("own", Some("/w"), false, No, true, 5),
                    ("busy", Some("/b"), true, Yes, true, 90),
                    ("stopped", None, false, No, true, 0),
                ],
                5,
                wait.clone(),
            ),
            (
                "empty pool: wait instead of creating",
                vec![],
                3,
                wait.clone(),
            ),
            (
                "a member of unknown state counts as stopped",
                vec![("free", None, false, Unknown, true, 0)],
                3,
                wait,
            ),
            (
                "at max_sandboxes with nothing free the wait is for a member",
                vec![("a", Some("/a"), true, Yes, true, 0)],
                1,
                Decision::Wait(WaitFor::Member),
            ),
        ];
        for (name, rows, max, expected) in cases {
            let members = candidates(&rows);
            let limits = Limits {
                max_sandboxes: max,
                ..LIMITS
            };
            assert_eq!(
                decide(&members, "/w", limits, false, NOW),
                expected,
                "{name}"
            );
        }
    }
}
