---
title: Limits and pools
description: How boatparty shares sandboxes between checkouts, spends boat's starts, and stops idle sandboxes.
order: 5
category: Guides
summary: Pool caps, the start ledger, idle stops and lease caps, all in account.toml.
---

All limits live in one file, `~/.config/boatparty/account.toml`, for every project on the machine. `boatparty limits` shows usage against each of them; `boatparty limits set` changes them without disturbing the file's comments.

```bash
boatparty limits
boatparty limits set account max_running=4 reserve_starts_per_day=20
boatparty limits set project app max_sandboxes=3 starts_per_hour=10
```

## Pools

A project owns at most `max_sandboxes` sandboxes (default 8). A workspace (one checkout) leases one at a time:

1. the sandbox it used last, if free;
2. a sandbox no workspace holds, preferring one that is running;
3. a new sandbox, if the pool is below its cap;
4. an idle sandbox of another workspace: first one whose directory no longer exists, then the least recently used one idle longer than `steal_after_minutes`, preferring a running one;
5. otherwise it waits up to `acquire_wait_seconds`, then fails with a message naming the limit.

Several commands from the same workspace may use its sandbox at once (a server and checks). Nothing is ever deleted because a directory is missing; `boatparty pool prune` deletes stopped sandboxes no workspace needs, after asking. `boatparty pool` shows every project's sandboxes, and `release` and `assign` steer them by hand.

## Starts

boat counts every create and every resume onto a new machine as a start, per minute, hour and day, and never queues one. boatparty sends every start through one local ledger shared by all projects and reconciled with boat's own counts:

- A start that would exceed a project's `starts_per_hour` or `starts_per_day`, or use the account's `reserve_starts_per_hour` and `reserve_starts_per_day`, waits with one log line.
- boat's own rate limit for the minute or hour window is retried after the delay boat names. Only the daily window fails, with a message that says so.
- **Paid starts.** When boat's extra starts are on (`boat billing extra-starts on`), boat bills a start past the plan's included starts ($0.03 to $0.07 each, at most a monthly cap) instead of refusing it. boatparty then starts anyway and says so on stderr (`starting ... is a paid start (about $0.03 on boat's next invoice)`); the account's reserves no longer hold it back, the projects' own caps still do, and boat still refuses past its hard caps. `[account] paid_starts = false` (or `boatparty limits set account paid_starts=false`) makes boatparty never use a paid start. `boatparty limits` shows whether the next start would be paid.
- **The ledger.** `boatparty limits starts` shows boatparty's own starts in the rolling minute, hour and day windows, per project, next to the account's totals from boat; the gap is starts made elsewhere (CI, other tools, other machines). It lists each start's time, a per-hour timeline, when the oldest start leaves each window, and how many of boatparty's starts leave the day window over the next hours, so you can plan around the rolling limits.
- `max_running` (for the account, and per project) caps sandboxes running at once; at the cap, an idle running sandbox is reassigned rather than another one started.

## Idle and lease times

| Key | Default | Meaning |
| --- | --- | --- |
| `lease_minutes` | 5 (at most 5) | While a command runs, the auto-stop stays this far ahead |
| `heartbeat_minutes` | 2 | How often a running command pushes the auto-stop forward |
| `idle_minutes` | 5 (at most 15) | After the last command ends, the sandbox stops this much later |

A sandbox whose files are still being restored after a resume is never stopped by boatparty, and its idle deadline is pushed past the restore. A client that dies leaves its sandbox running for at most about `lease_minutes`. Do not stop sandboxes between commands: the next command would pay a start, and a stopped sandbox costs nothing anyway.

## account.toml keys

| Table | Keys |
| --- | --- |
| `[provider]` | `name` ("boat"), `api_url` |
| `[defaults]` | `machine`, `max_sandboxes`, `steal_after_minutes`, `lease_minutes`, `idle_minutes`, `heartbeat_minutes`, `acquire_wait_seconds` |
| `[account]` | `max_running`, `reserve_starts_per_hour`, `reserve_starts_per_day`, `paid_starts` |
| `[projects.NAME]` | `max_sandboxes`, `max_running`, `max_machine`, `starts_per_hour`, `starts_per_day`, `idle_minutes`, `lease_minutes` |

A project's `boatparty.toml` may ask for less than its limits, never more: a `[sandbox] machine` above `max_machine` is an error naming both files.
