---
title: JSON output
description: Script boatparty and build tools on it with --json, using one JSON document per command or newline-delimited frames for streaming commands, stable error codes, and a contract that tests check on every change.
order: 44
category: Reference
summary: Documents, streaming frames, errors and where the full contract lives.
---

Every boatparty command accepts `--json`. Instead of human-readable text, it prints JSON on stdout that a script, a tool or a coding agent can parse reliably. The shapes are a contract: [`docs/json.md`](https://github.com/amxv/boatparty/blob/main/docs/json.md) in the repository documents every command's output and every error code, and a test checks each command's real output against it on every change.

This page explains the conventions with examples. Use the contract for the exact fields of each command.

## Turning it on

`--json` is a global option. Put it before the command's own arguments:

```bash
boatparty --json status
boatparty --json run -- npm test
boatparty --json limits
```

boatparty's own human-readable messages keep going to stderr (prefixed `boatparty: `), so stdout carries only JSON.

## Ordinary commands: one document

Commands that do not stream output print exactly one JSON document, pretty-printed, so read the whole of stdout before parsing. Success has `"ok": true`:

```bash
boatparty --json url
```

```json
{
  "forwards": [
    {
      "pid": 72373,
      "remote_port": 3000,
      "started_at": 1795717890116,
      "url": "http://localhost:50135"
    }
  ],
  "ok": true,
  "project": "hello-api",
  "workspace": "/Users/you/code/hello-api"
}
```

With `jq`:

```bash
boatparty --json status | jq -r '.members[] | "\(.name) \(.state)"'
boatparty --json limits | jq '.account'
```

## Streaming commands: frames

`run`, `serve`, `gate`, `pipe` and `shell -- COMMAND` print **newline-delimited frames** instead: one JSON object per line, in order, as things happen.

```bash
boatparty --json run -- node -e 'console.log("hi")'
```

```json
{"event":"preparing","type":"event"}
{"data":"boatparty: ensure npm-install (its check failed)","type":"stderr"}
{"data":"\n\nup to date, audited 1 package in 122ms\n\nfound 0 vulnerabilities\nboatparty: ensure npm-install done in 0s\n","type":"stderr"}
{"event":"started","type":"event"}
{"data":"hi\n","type":"stdout"}
{"cancelled":false,"command":"node -e 'console.log(\"hi\")'","command_exit_code":0,"copy_back":{"status":"none"},"exit_code":0,"kind":"run","ok":true,"seconds":1.491,"type":"result","warm":true, "...": "..."}
```

(The last line is shortened here.)

| Frame | Meaning |
| --- | --- |
| `{"type": "stdout", "data": "..."}` | Output the command wrote to stdout |
| `{"type": "stderr", "data": "..."}` | Output on stderr, including step output |
| `{"type": "event", "event": "..."}` | A runner event: `preparing` (steps start), `started` (the command started), `failed-step <section>.<name>`, `ready` (a server's ready port listens) |
| `{"type": "result", ...}` | Exactly one, always last: the outcome |

Output that is not valid UTF-8 travels as `"base64"` instead of `"data"`, and a frame never splits a UTF-8 character.

The `ready` event of `serve` also carries `urls`, the local forward URLs in argument order, so a script can wait for it and open the right URL:

```bash
boatparty --json serve --forward auto:3000 -- npm run dev |
  jq -r --unbuffered 'select(.type == "event" and .event == "ready") | .urls[0]'
```

### The result frame

The result of `run` tells you everything about the run, for example:

| Field | Meaning |
| --- | --- |
| `exit_code` | The process's exit status (what `boatparty` itself exits with) |
| `command_exit_code` | The remote command's own status, or `null` if it never reported one (for example after a cancel) |
| `cancelled` | Whether it was cancelled by a signal |
| `warm` | `true` when no provider call was needed before the command |
| `started_member` | Whether this run created or resumed the sandbox |
| `prep_seconds`, `seconds` | Time until the command started, and in total |
| `queued_seconds`, `waited_seconds`, `restore_wait_seconds` | Time waiting for a start, for another process bringing the sandbox up, and for a file restore |
| `cpu_steal_percent` | The host's CPU steal during the command |
| `recovery` | `null`, or the recovery action taken (`restarted`, `replaced`) |
| `reattached` | Whether the client reconnected to the command after a disconnect |
| `failed_step` | The setup or ensure step that failed, such as `ensure.deps` |
| `copy_back` | `status` is `none`, `applied`, `conflict` (with the kept `patch` path), `off` or `failed` |

See the contract for the complete list, and for the extra fields of `serve`, `gate`, `pipe` and `shell`.

## Errors

When boatparty itself fails, it prints an error document and exits 125:

```json
{
  "ok": false,
  "error": { "code": "daily_start_limit", "message": "..." }
}
```

For a streaming command, the error is the final frame, with `"type": "result"`. The `code` is stable; the `message` is for people. Match on codes. [Exit codes and errors](/docs/exit-codes) lists every code.

`doctor` with a failing check prints the error shape plus its `checks`, so you can see which ones failed.

## Exit status with --json

`--json` does not change exit statuses. A streaming command exits with the remote command's status (the result's `exit_code`), 125 for a boatparty failure, and 130 or 143 after SIGINT or SIGTERM.

## Times and numbers

- Times named `*_at` in state-derived objects are **milliseconds since the Unix epoch**.
- `at` and `built_at` strings are **RFC 3339** timestamps.
- Durations are **seconds**, as numbers.
- `null` means absent or unknown.

## Secrets in JSON

No JSON document carries a secret: no API key, GitHub token, signed URL or `pipe` input. The two exceptions are deliberate: `expose` and `desktop` return their private URL in a `url` field, because returning it is their whole purpose. Treat those like passwords.

## runs.jsonl

`boatparty log` reads `runs.jsonl` in boatparty's state directory: one JSON object per finished `run`, `serve` and `pipe`. It never holds credentials or `pipe` input. Its fields are documented in the [contract](https://github.com/amxv/boatparty/blob/main/docs/json.md#runsjsonl). `boatparty --json log` returns the same records.
