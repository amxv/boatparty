---
title: Exit codes and errors
description: What every boatparty exit status means, why 125 is reserved for boatparty's own failures, and the stable error codes that --json reports, with what to do about each group.
order: 45
category: Reference
summary: Exit statuses, the 125 contract and every stable error code.
---

boatparty is built so that a script or an agent can always tell whether **the command** failed or **boatparty** failed. That distinction lives in the exit status.

## Exit statuses

| Status | Meaning |
| --- | --- |
| The command's own | `run`, `serve`, `pipe`, `gate` and `shell -- COMMAND` exit with the remote command's status: 0 when it succeeded, anything else when it failed |
| 0 | Any other command succeeded |
| 1 | `url` found no matching running serve forward (error code `no_serve_forward`) |
| 2 | Usage error: an unknown option, a missing argument or a bad value, reported by the argument parser before anything runs |
| 125 | boatparty itself failed: configuration, credentials, provider, connection, limits. The command's result is unknown |
| 130 | Cancelled by SIGINT (Ctrl-C). The remote command was stopped too |
| 143 | Cancelled by SIGTERM. The remote command was stopped too (not on Windows) |

### Why 125

Commands use small exit codes for their own failures (1 for failing tests, 2 for usage errors, and so on). 125 is a code that ordinary programs almost never use (`docker run` uses it the same way, for failures of Docker itself). So when you see 125 from `boatparty run`, you know the remote command did not report a result: either it never ran, or its outcome is unknown.

The rule for scripts and agents:

```bash
boatparty run -- make test
status=$?
if [ "$status" -eq 125 ]; then
  echo "infrastructure problem: retry once, then report it"
elif [ "$status" -ne 0 ]; then
  echo "the tests failed: fix the code"
fi
```

A remote command that itself exits 125 is indistinguishable from a boatparty failure by status alone; with `--json`, the result frame's `ok` field tells them apart (`ok: true` with `exit_code: 125` means the command really returned 125).

## Error messages

In text mode, boatparty prints a failure on stderr as `boatparty: error: MESSAGE`, usually followed by advice:

```text
boatparty: error: cannot stop bp-app-1: it is in use by pid 72373 (npm run dev); wait for those commands to finish
```

With `--json`, a failure is a document (or, for a streaming command, a final result frame) with a stable `code`:

```json
{
  "ok": false,
  "error": { "code": "member_in_use", "message": "cannot stop bp-app-1: it is in use by pid 72373 (npm run dev); wait for those commands to finish" }
}
```

The `code` is stable across versions; the `message` is for people and may change. Match on codes in scripts.

## What to do, by group

| Codes | What happened | What to do |
| --- | --- | --- |
| `config`, `workspace`, `unknown_command`, `unknown_preset`, `invalid_limit`, `usage` | Something in the command line, `boatparty.toml`, `account.toml` or the environment is wrong | Read the message; it names the file and key. Fix it and rerun. `boatparty config validate` helps |
| `credentials`, `unauthorized`, `key_scope_forbidden`, `forbidden` | The API key is missing, invalid, expired or lacks a permission | `boatparty login --create-key`, then `boatparty doctor` |
| `connection`, `ssh`, `step_timeout`, `run_failed`, `sync_failed`, `fetch_failed` | The connection to the sandbox or the API failed | Retry once. If it repeats, `boatparty doctor` and `boatparty status` |
| `pool_exhausted`, `running_limit`, `daily_start_limit`, `admission_timeout`, `start_rate_limited` | A limit was reached | `boatparty limits` and `boatparty pool`; wait, free sandboxes, or raise the limit. See [Starts, limits and costs](/docs/limits) |
| `member_in_use`, `already_serving`, `gate_busy`, `base_busy` | Something else is using the sandbox, port or gate | Wait for it, reuse it, or stop the holder named in the message |
| `restore_stalled`, `ssh_unreachable`, `start_timeout`, `hydration_timeout`, `degraded_timeout`, `tunnel_degraded` | The sandbox is slow or stuck | Retry; boatparty recovers stuck members automatically. See [Reliability and recovery](/docs/reliability) |
| `provider_at_capacity`, `concurrency_limit`, `billing`, `provider_error`, `provider_busy` | Boat cannot serve the request right now, or the account needs attention | Retry later; check your boat account and billing |
| `no_serve_forward` | `url` found nothing (exit 1) | Start a server with `serve --forward`, or check the directory |

See [Troubleshooting](/docs/troubleshooting) for step-by-step fixes.

## Every error code

Every failure of boatparty itself exits 125 (except `no_serve_forward`, which exits 1) with one of these codes. This table mirrors the [JSON contract](https://github.com/amxv/boatparty/blob/main/docs/json.md#error-codes), which is the source of truth.

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
| `no_serve_forward` | No running serve forward matches this workspace and port (exit 1) |
| `already_serving` | A live serve already claims the sandbox port |
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
| `desktop_failed` | The streaming services failed to start or the local browser failed to open |
| `hide_failed` | `expose --hide` failed on the sandbox |
| `prune_failed` | `pool prune` could not delete every candidate |
| `gate_busy` | Another process holds the gate member |
| `base_busy`, `base_build_failed`, `base_save_failed` | Another base build runs; the build or the save failed |
| `interrupted` | SIGINT or SIGTERM arrived before the work started (the exit status is then 130 or 143) |
| `not_found` | The provider has no such sandbox or snapshot |
| `start_rate_limited`, `request_rate_limited` | The provider's start or request rate limit (starts normally wait instead) |
| `provider_at_capacity`, `concurrency_limit`, `billing` | The provider has no machine, the account's concurrent limit is reached, or the account needs payment |
| `restore_stalled`, `ssh_unreachable` | File restore stopped progressing or repeated direct SSH probes failed |
| `step_timeout`, `degraded_timeout` | A named connection or sync step exceeded its deadline. A degraded member is marked for recovery |
| `admission_timeout` | A start could not be admitted within 65 minutes |
| `tunnel_degraded` | A tunnel-dependent operation could not safely repair the degraded tunnel |
| `provider_busy`, `not_running`, `conflict` | The sandbox is restoring or securing, has no running machine, or is in a conflicting state |
| `unauthorized`, `key_scope_forbidden`, `forbidden` | The key is invalid or expired, lacks an action, or the request is refused |
| `invalid_request`, `too_large`, `provider_error`, `invalid_response`, `provider_unknown_error` | The provider rejected the request, failed, answered unreadably, or returned an unknown error code |
