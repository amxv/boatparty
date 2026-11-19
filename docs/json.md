# boatparty JSON output

This file is the contract for `--json`. Scripts, tools built on boatparty and the agent skill rely on it. A shape changes only together with this file, in the same commit, and `tests/json.rs` checks every command's output against the examples below: the top-level fields must be exactly the documented ones, and nested objects may hold only documented fields. A `"*"` key marks an object whose keys are free (a map). Values in the examples are illustrative; `null` stands for "absent or not known".

## Conventions

- `--json` is a global option: put it before the command's own arguments (`boatparty --json run -- make`).
- Non-streaming commands print exactly one JSON document on stdout (pretty-printed, so read the whole of stdout). Success has `"ok": true`.
- Streaming commands (`run`, `serve`, `pipe`, `shell -- COMMAND`) print newline-delimited frames, one JSON object per line, in order:
  - `{"type": "stdout", "data": "..."}` and `{"type": "stderr", "data": "..."}` as output arrives. Output that is not valid UTF-8 travels as `"base64"` instead of `"data"`; a frame never splits a UTF-8 sequence.
  - `{"type": "event", "event": "started"}` and the other runner events: `preparing` (setup or ensure steps start), `failed-step <section>.<name>`, `ready` (a `serve --ready-port` listens).
  - Exactly one `{"type": "result", ...}`, always last.
- A failure of boatparty itself is `{"ok": false, "error": {"code": "...", "message": "..."}}`, with `"type": "result"` for a streaming command, and exit status 125. `code` is stable (see [Error codes](#error-codes)); `message` is for people.
- The exit status of a streaming command is the remote command's (the result's `exit_code`), 125 for a failure of boatparty, 130 or 143 after SIGINT or SIGTERM.
- Times called `*_at` in state-derived objects are milliseconds since the epoch; `at` and `built_at` strings are RFC 3339. Durations are seconds as numbers.
- No document carries a secret (the API key, GitHub tokens, signed URLs, `pipe` input). The one deliberate exception is `expose`, whose `url` may carry an access token.
- boatparty's own messages always go to stderr, prefixed `boatparty: `.

On Windows, local paths are Windows paths. Remote paths use Linux syntax. Console Ctrl-C returns 130. The `control_sockets` doctor check reports `skipped` because Windows OpenSSH uses direct connections.

## Errors

<!-- shape: error -->
```json
{
  "ok": false,
  "error": { "code": "billing", "message": "the account needs a payment: ..." }
}
```

A streaming command's failure is its result frame:

<!-- shape: error frame -->
```json
{
  "type": "result",
  "ok": false,
  "error": { "code": "connection", "message": "the session on bp-app-1 ended without a result (ssh exit 255); ..." }
}
```

`doctor` with a failed check prints the error shape plus its `checks` (see `doctor`).

## Commands run anywhere

### login

`boatparty login --key-stdin [--no-verify]`

<!-- shape: login -->
```json
{
  "ok": true,
  "credentials_file": "/Users/me/.config/boatparty/credentials.toml",
  "verified": true,
  "overridden_by_environment": false
}
```

### doctor

`status` is `ok`, `warn` (worth cleaning up, not a failure: stale SSH files and control sockets), `fail` or `skipped`. `fix` is the command that fixes a `warn` or `fail`. Check names: `ssh`, `ssh-keygen`, `git`, `account`, `credentials`, `key`, `key_scope`, `pools` (reconciliation with the provider), `ssh_configs`, `control_sockets`. When any check fails, the document is the error shape (`code` `doctor_failed`) with the same `checks` added, and the exit status is 125.

<!-- shape: doctor -->
```json
{
  "ok": true,
  "checks": [
    { "name": "ssh_configs", "status": "warn", "detail": "1 stale file for sandboxes no record names: bx_gone.conf", "fix": "rm -f /Users/me/.local/state/boatparty/ssh/bx_gone.conf" }
  ]
}
```

### limits

Provider figures first (`starts` per window, `sandboxes` running against the account's cap, included VM time, sizes), then boatparty's own: `account` (its limits and the starts boatparty made) and one entry per project. `projects[].starts.*.cap` is the project's `starts_per_hour` or `starts_per_day`; `cost` is this month's machine cost from the provider. `extra_starts` is true when boat bills starts past the plan's limits instead of refusing them (extra starts on, below their monthly cap); `next_start_dollars` is then what the next start costs. `account.paid_starts` is `[account] paid_starts` (default true): whether boatparty uses such paid starts.

<!-- shape: limits -->
```json
{
  "ok": true,
  "provider": "boat",
  "can_start": true,
  "start_blocked_reason": null,
  "starts": {
    "unlimited": false,
    "minute": { "limit": 12, "used": 0, "remaining": 12 },
    "hour": { "limit": 60, "used": 8, "remaining": 52 },
    "day": { "limit": 200, "used": 50, "remaining": 150 }
  },
  "sandboxes": { "running": 2, "max_running": 100 },
  "included_vm_time": { "hours": 555.56, "hours_remaining": 450.5, "period_end": "2026-12-01T00:00:00.000Z" },
  "sizes": [ { "size": "xlarge", "available": false, "reason": "needs the $100/month plan or higher" } ],
  "next_start_dollars": 0.0,
  "extra_starts": false,
  "overage": { "vm_hour_dollars": 0.036, "saved_snapshot_day_dollars": 0.056 },
  "account": {
    "file": "/Users/me/.config/boatparty/account.toml",
    "max_running": null,
    "reserve_starts_per_hour": 0,
    "reserve_starts_per_day": 0,
    "paid_starts": true,
    "boatparty_starts": { "hour": 0, "day": 0 }
  },
  "projects": [
    {
      "project": "app",
      "members": 1,
      "running": 1,
      "machine": "default",
      "max_machine": null,
      "max_sandboxes": 8,
      "max_running": null,
      "idle_minutes": 5,
      "lease_minutes": 5,
      "starts": { "hour": { "used": 0, "cap": null }, "day": { "used": 0, "cap": null } },
      "cost": { "dollars": 0.42, "seconds": 41760, "since": "2026-11-01T00:00:00Z" },
      "error": null
    }
  ]
}
```

### limits starts

`boatparty limits starts`: boatparty's start ledger (every create and new-machine resume it made from this machine in the last 24 hours) beside the account's figures from the provider. Per window (`minute`, `hour`, `day`): `boatparty` and its split by project, `account` (`null` when the provider gave no figure), `outside_boatparty` (account starts boatparty did not make, such as CI or other tools; `null` without a figure) and `next_frees_at` (when boatparty's oldest start in the window leaves it). `starts` lists each start, oldest first (`in_flight` while its answer is pending). `frees_by_hour` covers the next 24 hours: how many of boatparty's starts leave the day window by each `until`. `provider_window_full` is the provider's full window and when it frees, if any.

<!-- shape: limits starts -->
```json
{
  "ok": true,
  "now": "2026-11-15T12:00:00Z",
  "windows": [
    {
      "window": "day",
      "boatparty": 40,
      "projects": { "app": 31, "web": 9 },
      "account": { "limit": 200, "used": 120, "remaining": 80 },
      "outside_boatparty": 80,
      "next_frees_at": "2026-11-15T12:20:00Z"
    }
  ],
  "starts": [ { "at": "2026-11-14T12:20:00Z", "project": "app", "in_flight": false } ],
  "frees_by_hour": [ { "until": "2026-11-15T13:00:00Z", "frees": 3 } ],
  "provider_window_full": { "window": "day", "frees_at": "2026-11-15T13:35:21.676Z" },
  "extra_starts": true,
  "next_start_dollars": 0.03
}
```

### limits set

`boatparty limits set account KEY=VALUE...` or `limits set project NAME KEY=VALUE...`. `table` is the account.toml table changed (`account`, `defaults` or `projects.NAME`); `new` is `null` when an empty value removed the key. `project` is `null` for `account`.

<!-- shape: limits set -->
```json
{
  "ok": true,
  "scope": "project",
  "project": "app",
  "file": "/Users/me/.config/boatparty/account.toml",
  "changes": [ { "table": "projects.app", "key": "max_sandboxes", "old": null, "new": 3 } ]
}
```

### pool

`boatparty pool [--refresh] [--project NAME]`. `changes` is `null` without `--refresh`; with it, what reconciliation did (`freed` holds `[member, workspace]` pairs). `untracked` lists sandboxes with the project's label but no record. `provider_error` says why the provider's states are missing (members' `state` is then `unknown`).

<!-- shape: pool -->
```json
{
  "ok": true,
  "refreshed": true,
  "projects": [
    {
      "project": "app",
      "max_sandboxes": 8,
      "steal_after_minutes": 30,
      "members": [
        {
          "name": "bp-app-1",
          "id": "bx_23456789",
          "role": "pool",
          "state": "running",
          "affinity": "/Users/me/code/app",
          "workspace_exists": true,
          "leases": [ { "pid": 4242, "kind": "run", "label": "cargo test", "workspace": "/Users/me/code/app", "started_at": 1794730331934 } ],
          "bringing_up": { "pid": 4242, "started_at": 1794730331934 },
          "created_at": 1794730331934,
          "last_used_at": 1794730332265,
          "last_run": { "at": "2026-11-15T08:12:12Z", "kind": "run", "command": "cargo test", "exit_code": 0, "seconds": 5.2 },
          "broken": null
        }
      ],
      "untracked": [ { "id": "bx_34567890", "name": "bp-app-2", "state": "stopped" } ],
      "provider_error": null,
      "changes": {
        "adopted": [ { "name": "bp-app-2", "id": "bx_34567890" } ],
        "dropped": [ { "name": "bp-app-3", "id": "bx_45678901" } ],
        "expired": [ { "name": "bp-app-4", "id": "bx_56789012" } ],
        "freed": [ ["member", "workspace"] ],
        "errors": [ "bp-app-5: ..." ]
      }
    }
  ]
}
```

### pool prune

`boatparty pool prune [--yes] [--project NAME]`. `aborted` is true when the question was answered no; then nothing was deleted. `skipped` lists candidates that turned out not to be deletable any more.

<!-- shape: pool prune -->
```json
{
  "ok": true,
  "candidates": [ { "project": "app", "name": "bp-app-2", "id": "bx_34567890", "affinity": null } ],
  "deleted": [ { "project": "app", "name": "bp-app-2", "id": "bx_34567890", "affinity": null } ],
  "skipped": [ { "project": "app", "name": "bp-app-3", "id": "bx_45678901", "affinity": null } ],
  "aborted": false
}
```

### log

`boatparty log [-n N]`: the last N records of `runs.jsonl`, oldest first, as stored (see [runs.jsonl](#runsjsonl)).

<!-- shape: log -->
```json
{
  "ok": true,
  "runs": [ { "*": "one runs.jsonl record" } ]
}
```

### skill

`boatparty skill` writes the skill and returns its path; nothing is installed.

<!-- shape: skill -->
```json
{
  "ok": true,
  "path": "/Users/me/.local/state/boatparty/skill/boatparty/SKILL.md",
  "version": "0.1.0",
  "bytes": 4100
}
```

`boatparty skill --print` returns the text instead of writing it:

<!-- shape: skill --print -->
```json
{
  "ok": true,
  "version": "0.1.0",
  "content": "---\nname: boatparty\n..."
}
```

## Commands run in a workspace

`workspace` is always the absolute path of the checkout's Git toplevel; `member` names the sandbox (`bp-<project>-<n>`) and its provider id.

### init

`presets`, `commands` (name to shell line) and `detected_from` (the marker files) are `null` when the file already existed. `base` is as in `status`.

<!-- shape: init -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "file": "/Users/me/code/app/boatparty.toml",
  "file_created": true,
  "presets": ["rust"],
  "commands": { "*": "shell line" },
  "detected_from": ["Cargo.toml"],
  "registered": true,
  "account_file": "/Users/me/.config/boatparty/account.toml",
  "base": { "present": false, "stale": null, "reasons": [] }
}
```

### status

One entry per member of this project. `state` is `starting`, `running`, `stopping`, `stopped`, `error`, `gone`, `pending` (create not answered yet) or `unknown`; `hydration` is `complete`, `in_progress` or `unknown`. `ssh` gives what scripts need to reach a member with plain `ssh -F <config> <host_alias>`. `broken` is why boatparty will replace the member at its next start (a file restore that stalled for 10 minutes, or a machine the provider could not reach for as long), else `null`. `base` summarizes the project's base snapshot.

<!-- shape: status -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "members": [
    {
      "name": "bp-app-1",
      "id": "bx_23456789",
      "role": "pool",
      "state": "running",
      "hydration": "complete",
      "size": "default",
      "affinity": "/Users/me/code/app",
      "this_workspace": true,
      "auto_stop_at": "2026-11-15T12:15:00Z",
      "leases": [ { "pid": 4242, "kind": "run", "label": "cargo test", "started_at": 1794730331934 } ],
      "bringing_up": { "pid": 4242, "started_at": 1794730331934 },
      "ssh": {
        "host_alias": "bx_23456789",
        "config": "/Users/me/.local/state/boatparty/ssh/bx_23456789.conf",
        "known_hosts": "/Users/me/.local/state/boatparty/ssh/bx_23456789.known_hosts",
        "configured": true
      },
      "created_at": 1794730331934,
      "last_used_at": 1794730332265,
      "broken": null
    }
  ],
  "untracked": [ { "id": "bx_34567890", "name": "bp-app-2", "state": "stopped" } ],
  "base": {
    "present": true,
    "snapshot": "bp-app",
    "commit": "20f65ed9efbe2f0d39788951ecc73c53c3544a8a",
    "built_at": "2026-11-15T08:00:00Z",
    "stale": false,
    "reasons": []
  }
}
```

### start

`member` is a `status` member plus how this call brought it up.

<!-- shape: start -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": {
    "name": "bp-app-1",
    "id": "bx_23456789",
    "role": "pool",
    "state": "running",
    "hydration": "complete",
    "size": "default",
    "affinity": "/Users/me/code/app",
    "this_workspace": true,
    "auto_stop_at": "2026-11-15T12:15:00Z",
    "leases": [ { "pid": 4242, "kind": "run", "label": "cargo test", "started_at": 1794730331934 } ],
    "bringing_up": { "pid": 4242, "started_at": 1794730331934 },
    "ssh": { "host_alias": "bx_23456789", "config": "/Users/me/.local/state/boatparty/ssh/bx_23456789.conf", "known_hosts": "/Users/me/.local/state/boatparty/ssh/bx_23456789.known_hosts", "configured": true },
    "created_at": 1794730331934,
    "last_used_at": 1794730332265,
    "broken": null,
    "created": false,
    "resumed": true,
    "reclaimed": false,
    "reconnected": true
  }
}
```

### sync

`pack_route` is `none`, `ssh` or `signed_url`; `upload_fallback` says why a signed upload failed. `seed_route` is `none` or `github`; `seed_fallback` says why seeding from GitHub failed, `seed_skipped` why it was not tried. `full_retry` is true when the sandbox lacked objects and a full pack was sent.

<!-- shape: sync -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "checkout": "/home/user/app",
  "head": "20f65ed9efbe2f0d39788951ecc73c53c3544a8a",
  "branch": "main",
  "wip": "f6aa59c2a316a7f92b95fac03cb0e561cf755d91",
  "tree": "4d43ebca55e40d2fd61c41bbb67d93324bdeed17",
  "history": "head",
  "pack_bytes": 3152,
  "pack_route": "ssh",
  "upload_fallback": null,
  "seed_route": "none",
  "seed_fallback": null,
  "seed_skipped": "the repository has no remote to seed from",
  "seed_seconds": 0.0,
  "full_retry": false,
  "runner_installed": false,
  "seconds": 0.7
}
```

### run

Frames, then this result. `exit_code` is the process's exit status; `command_exit_code` is the remote command's (`null` when it never reported one, for example after a cancel). `prep_seconds` runs from the start until the command started. `warm` means no provider call was needed; `started_member` that this run created or resumed the sandbox; `retried` that it ran again under the ssh-255 rule. `failed_step` names a setup or ensure step that failed (`ensure.deps`), in which case the command did not run. `git_identity` and `github_token` say which credentials the command got. `copy_back.status` is `none`, `applied`, `conflict` (the patch is kept at `patch`), `off` (`[sync] copy_back = false`) or `failed` (with `error`); `applied` and `conflict` add `route`, how the changes came back: `ssh`, or `signed_url` for a patch above the signed-transfer threshold (512 KiB).

<!-- shape: run -->
```json
{
  "type": "result",
  "ok": true,
  "kind": "run",
  "exit_code": 0,
  "command_exit_code": 0,
  "cancelled": false,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "command": "cargo test --locked",
  "seconds": 5.7,
  "prep_seconds": 0.8,
  "warm": true,
  "started_member": false,
  "retried": false,
  "pack_bytes": 1210,
  "pack_route": "ssh",
  "upload_fallback": null,
  "seed_route": "none",
  "seed_fallback": null,
  "seed_skipped": null,
  "seed_seconds": 0.0,
  "full_retry": false,
  "runner_installed": false,
  "failed_step": null,
  "git_identity": true,
  "github_token": false,
  "copy_back": { "status": "conflict", "files": ["src/lib.rs"], "patch": "/Users/me/code/app/.git/boatparty-pullback-1794730331934.patch", "error": null, "route": "ssh" }
}
```

### serve

Frames, then the `run` result with `kind` `serve` and the server's settings. `forwards` and `reverses` list the port forwards; `capped` is true when `--max-minutes` stopped the server; `ready_seconds` is when `--ready-port` started listening.

<!-- shape: serve -->
```json
{
  "type": "result",
  "ok": true,
  "kind": "serve",
  "exit_code": 130,
  "command_exit_code": null,
  "cancelled": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "command": "npm run dev",
  "seconds": 600.2,
  "prep_seconds": 1.4,
  "warm": false,
  "started_member": false,
  "retried": false,
  "pack_bytes": 0,
  "pack_route": "none",
  "upload_fallback": null,
  "seed_route": "none",
  "seed_fallback": null,
  "seed_skipped": null,
  "seed_seconds": 0.0,
  "full_retry": false,
  "runner_installed": false,
  "failed_step": null,
  "git_identity": true,
  "github_token": false,
  "copy_back": { "status": "none", "files": [], "patch": null, "error": null, "route": null },
  "forwards": [ { "bind": "127.0.0.1", "listen": 3000, "target": 3000 } ],
  "reverses": [ { "bind": "127.0.0.1", "listen": 20000, "target": 7777 } ],
  "ready_port": 3000,
  "max_minutes": 120.0,
  "capped": false,
  "ready_seconds": 4.1
}
```

### shell

`boatparty --json shell -- COMMAND`: frames, then this result. `shell` without a command has no JSON form (error `usage`). `git_identity` and `github_token` say whether the command got the local Git identity and a GitHub token (both false when the runner could not prepare the session, which a stderr frame reports).

<!-- shape: shell -->
```json
{
  "type": "result",
  "ok": true,
  "kind": "shell",
  "exit_code": 0,
  "project": "app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "command": "uname -a",
  "git_identity": true,
  "github_token": false,
  "seconds": 0.4
}
```

### pipe

Frames, then this result. `input_bytes` is how much input the command was sent; the input itself is never recorded.

<!-- shape: pipe -->
```json
{
  "type": "result",
  "ok": true,
  "kind": "pipe",
  "exit_code": 0,
  "command_exit_code": 0,
  "cancelled": false,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "command": "bash scripts/upload.sh",
  "seconds": 0.5,
  "input_bytes": 412,
  "runner_installed": false,
  "git_identity": true,
  "github_token": false
}
```

### fetch

`kind` is `file` or `directory`; `files` and `bytes` count what arrived; `route` is `ssh` or `signed_url`, and `download_fallback` says why a signed download failed.

<!-- shape: fetch -->
```json
{
  "ok": true,
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "remote_path": "/home/user/app/target/release/app",
  "local_path": "/Users/me/code/app/app-linux",
  "kind": "file",
  "files": 1,
  "bytes": 3746560,
  "archive_bytes": 1402233,
  "route": "signed_url",
  "download_fallback": null,
  "seconds": 2.7
}
```

### forward

`boatparty --json forward REMOTE[:LOCAL]` prints this once the forward listens, then holds it until SIGINT or SIGTERM (exit 130 or 143). `listen` is the local port, `target` the sandbox port. A failure before it listens is the error shape.

<!-- shape: forward -->
```json
{
  "ok": true,
  "project": "app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "forward": { "bind": "127.0.0.1", "listen": 15432, "target": 5432 },
  "ready_seconds": 0.6
}
```

### expose

`url` may carry an access token (`_token`): treat it like a password. It is printed here and nowhere else. `access` is `private` (token-gated) or `public`.

<!-- shape: expose -->
```json
{
  "ok": true,
  "project": "app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "port": 8000,
  "url": "https://app-8000.on.boat.example?_token=...",
  "protected": true,
  "access": "private",
  "localhost": false
}
```

`boatparty expose PORT --hide`:

<!-- shape: expose --hide -->
```json
{
  "ok": true,
  "project": "app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "port": 8000,
  "hidden": true
}
```

### release

`released` is false (and `member` `null`) when the workspace held no sandbox.

<!-- shape: release -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "released": true,
  "member": { "name": "bp-app-1", "id": "bx_23456789" }
}
```

### assign

`previous` is the sandbox this workspace held before, now free; `unchanged` is true when `MEMBER` already served it.

<!-- shape: assign -->
```json
{
  "ok": true,
  "project": "app",
  "workspace": "/Users/me/code/app",
  "member": { "name": "bp-app-2", "id": "bx_34567890" },
  "previous": { "name": "bp-app-1", "id": "bx_23456789" },
  "unchanged": false
}
```

### stop

`member` is a `status` member as of the stop request (with `--wait`, once stopped). `stop_requested` is false when it was already stopping or stopped.

<!-- shape: stop -->
```json
{
  "ok": true,
  "project": "app",
  "stop_requested": true,
  "member": {
    "name": "bp-app-1",
    "id": "bx_23456789",
    "role": "pool",
    "state": "stopping",
    "hydration": "complete",
    "size": "default",
    "affinity": "/Users/me/code/app",
    "this_workspace": true,
    "auto_stop_at": "2026-11-15T12:15:00Z",
    "leases": [ { "pid": 4242, "kind": "run", "label": "cargo test", "started_at": 1794730331934 } ],
    "bringing_up": { "pid": 4242, "started_at": 1794730331934 },
    "ssh": { "host_alias": "bx_23456789", "config": "/Users/me/.local/state/boatparty/ssh/bx_23456789.conf", "known_hosts": "/Users/me/.local/state/boatparty/ssh/bx_23456789.known_hosts", "configured": true },
    "created_at": 1794730331934,
    "last_used_at": 1794730332265,
    "broken": null
  }
}
```

### rm

`deleted` is false (and `deletion` `null`) when only a record without a sandbox was dropped.

<!-- shape: rm -->
```json
{
  "ok": true,
  "project": "app",
  "member": { "name": "bp-app-1", "id": "bx_23456789" },
  "deleted": true,
  "deletion": { "operation_id": "bdop_1", "status": "completed" }
}
```

### base build

`base` is the recorded base; `builder` the builder sandbox and how it was brought up; `phases` the seconds each step took, by name.

<!-- shape: base build -->
```json
{
  "ok": true,
  "project": "app",
  "base": {
    "snapshot": "bp-app",
    "commit": "20f65ed9efbe2f0d39788951ecc73c53c3544a8a",
    "known": ["20f65ed9efbe2f0d39788951ecc73c53c3544a8a"],
    "refs_digest": "6a5f1f0e0b1c2d3e",
    "built_at": 1794730331934,
    "builder_id": "bx_b",
    "mode": "head",
    "ref": "refs/remotes/origin/main",
    "branch": "main",
    "size_bytes": 16777216
  },
  "builder": { "name": "bp-app-builder", "id": "bx_b", "created": false, "resumed": true },
  "pack_bytes": 177,
  "seed_route": "none",
  "phases": { "*": "seconds" },
  "seconds": 124.0
}
```

### base status

`base` is the record as in `base build` (`null` without one). `saved` is the saved snapshot at the provider; `staleness` compares the base with its ref now (`behind` in commits, `inputs_changed` lists changed `stale_inputs`); `allowance` is the account's saved-snapshot allowance.

<!-- shape: base status -->
```json
{
  "ok": true,
  "project": "app",
  "snapshot": "bp-app",
  "base": {
    "snapshot": "bp-app",
    "commit": "20f65ed9efbe2f0d39788951ecc73c53c3544a8a",
    "known": ["20f65ed9efbe2f0d39788951ecc73c53c3544a8a"],
    "refs_digest": "6a5f1f0e0b1c2d3e",
    "built_at": 1794730331934,
    "builder_id": "bx_b",
    "mode": "head",
    "ref": "refs/remotes/origin/main",
    "branch": "main",
    "size_bytes": 16777216
  },
  "built_at": "2026-11-15T08:00:00Z",
  "saved": { "name": "bp-app", "status": "ready", "error": null, "size_bytes": 16777216, "created_at": "2026-11-15T08:00:00Z", "source_id": "bx_b" },
  "staleness": {
    "age_ms": 3600000,
    "ref": "refs/remotes/origin/main",
    "ref_commit": "9a1b2c3d4e5f60718293a4b5c6d7e8f901234567",
    "behind": 2,
    "inputs_changed": ["Cargo.lock"],
    "stale": true,
    "reasons": ["2 commits behind refs/remotes/origin/main"]
  },
  "stale": true,
  "allowance": { "included": 10, "used": 1, "extra": 0, "extra_monthly_dollars": 0.0, "price_per_extra_daily_dollars": 0.056 },
  "builder": { "name": "bp-app-builder", "id": "bx_b" }
}
```

### base rm

<!-- shape: base rm -->
```json
{
  "ok": true,
  "project": "app",
  "snapshot": "bp-app",
  "snapshot_deleted": true,
  "record_removed": true,
  "builder_deleted": { "name": "bp-app-builder", "id": "bx_b" }
}
```

### config show

`boatparty config show`: the file as parsed (`view` `file`). `config` holds the boatparty.toml tables; see `boatparty config --help` for the schema.

<!-- shape: config show -->
```json
{
  "ok": true,
  "view": "file",
  "project": "app",
  "file": "/Users/me/code/app/boatparty.toml",
  "presets": ["rust"],
  "config": { "*": "boatparty.toml as parsed" }
}
```

`config show --resolved` (`view` `resolved`) adds the steps switched off by name:

<!-- shape: config show --resolved -->
```json
{
  "ok": true,
  "view": "resolved",
  "project": "app",
  "file": "/Users/me/code/app/boatparty.toml",
  "presets": ["rust"],
  "switched_off": ["ensure.cargo-fetch"],
  "config": { "*": "the effective configuration" }
}
```

`config show --origin` (which implies `--resolved`) also maps each dotted key to the layer it came from (`default`, a preset, `boatparty.toml`):

<!-- shape: config show --origin -->
```json
{
  "ok": true,
  "view": "resolved",
  "project": "app",
  "file": "/Users/me/code/app/boatparty.toml",
  "presets": ["rust"],
  "switched_off": [],
  "config": { "*": "the effective configuration" },
  "origins": { "*": "layer" }
}
```

### config validate

<!-- shape: config validate -->
```json
{
  "ok": true,
  "project": "app",
  "file": "/Users/me/code/app/boatparty.toml",
  "presets": ["rust"],
  "preset_files_checked": ["/Users/me/code/app/.boatparty/presets/rust.toml"]
}
```

### preset list

`source` is `built-in`, `user` or `project`; `shadows` lists the less specific layers a file replaces; `used` says whether this project names it.

<!-- shape: preset list -->
```json
{
  "ok": true,
  "user_dir": "/Users/me/.config/boatparty/presets",
  "project_dir": "/Users/me/code/app/.boatparty/presets",
  "presets": [
    { "name": "rust", "source": "project", "path": "/Users/me/code/app/.boatparty/presets/rust.toml", "summary": "Rust with Cargo: ...", "shadows": ["built-in"], "used": true }
  ]
}
```

### preset show

<!-- shape: preset show -->
```json
{
  "ok": true,
  "name": "rust",
  "source": "built-in",
  "path": null,
  "text": "# Rust with Cargo: ...\nversion = 1\n..."
}
```

### preset eject

`scope` is `project` or `user`; `replaced` is true with `--force` over an existing file; `shadowed_by` names a more specific file that will still win.

<!-- shape: preset eject -->
```json
{
  "ok": true,
  "name": "rust",
  "scope": "project",
  "path": "/Users/me/code/app/.boatparty/presets/rust.toml",
  "replaced": false,
  "shadowed_by": null
}
```

## runs.jsonl

`~/.local/state/boatparty/runs.jsonl` holds one JSON object per finished `run`, `serve` and `pipe`, appended; `log` returns them. It never holds credentials or `pipe` input. Common fields: `at` (RFC 3339), `kind` (`run`, `serve` or `pipe`), `command`, `project`, `workspace`, `member`, `sandbox`, `exit_code` (`null` when the command reported none), `error` (an error code, when boatparty failed), `cancelled`, `seconds`, `git_identity`, `github_token`. `run` and `serve` records add `name` (the `[commands]` name, if one was used), `warm`, `started_member`, `retried`, `prep_seconds`, `pack_bytes`, `pack_route`, `seed_route`, `seed_fallback`, `copy_back` (the status word) and `failed_step`; `serve` records add `capped` and `ready_seconds`; `pipe` records add `input_bytes`.

## Error codes

Every failure of boatparty itself exits 125 with one of these codes. Codes are stable; messages are not.

| Code | Meaning |
| --- | --- |
| `config` | A configuration file, the environment or the command line is invalid (account.toml, boatparty.toml, `BOATPARTY_HOME`) |
| `credentials` | No usable API key, or it cannot be stored |
| `io` | A local file operation failed |
| `usage` | A combination the command cannot do (for example `shell --json` without a command) |
| `workspace` | Not inside a Git working tree, no commit yet, or no boatparty.toml |
| `unknown_command` | `run NAME` names no `[commands]` entry and there is no script runner |
| `unknown_preset` | A preset name that no layer provides |
| `invalid_limit` | `limits set` got an unknown key or a bad value |
| `doctor_failed` | At least one `doctor` check failed (the document also carries `checks`) |
| `no_member` | This workspace has no sandbox, or a named sandbox or project is unknown |
| `member_in_use` | The sandbox has a live lease or another process is bringing it up |
| `member_pending` | The sandbox's create has no answer yet |
| `member_failed` | The sandbox is in the provider's error state |
| `member_assigned` | `assign` named a sandbox another existing workspace holds |
| `pool_exhausted` | No sandbox became free within `acquire_wait_seconds` |
| `running_limit` | `max_running` stayed reached for the whole wait, so no sandbox could start |
| `daily_start_limit` | The day's start window (boat's, or the project's `starts_per_day`) is full |
| `start_timeout`, `hydration_timeout`, `stop_timeout`, `deletion_timeout` | The provider did not reach the expected state in time |
| `data_expired` | The stopped sandbox's stored disk has expired |
| `ssh` | Setting up SSH (key, config, upload) failed |
| `connection` | The connection to the sandbox or to the API failed (for commands: ssh exit 255 without a result) |
| `sync_failed` | The working tree could not be mirrored |
| `run_failed` | The runner ended without reporting the command's result |
| `fetch_failed` | `fetch` could not copy the path back |
| `port_in_use` | A local port a forward needs is taken |
| `hide_failed` | `expose --hide` failed on the sandbox |
| `prune_failed` | `pool prune` could not delete every candidate |
| `base_busy`, `base_build_failed`, `base_save_failed` | Another base build runs; the build or the save failed |
| `interrupted` | SIGINT or SIGTERM arrived before the work started (the exit status is then 130 or 143) |
| `not_found` | The provider has no such sandbox or snapshot |
| `start_rate_limited`, `request_rate_limited` | The provider's start or request rate limit (starts normally wait instead) |
| `provider_at_capacity`, `concurrency_limit`, `billing` | The provider has no machine, the account's concurrent limit is reached, or the account needs payment |
| `provider_busy`, `not_running`, `conflict` | The sandbox is restoring or securing, has no running machine, or is in a conflicting state |
| `unauthorized`, `key_scope_forbidden`, `forbidden` | The key is invalid or expired, lacks an action, or the request is refused |
| `invalid_request`, `too_large`, `provider_error`, `invalid_response`, `provider_unknown_error` | The provider rejected the request, failed, answered unreadably, or returned an unknown error code |
