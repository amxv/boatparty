---
title: Changelog
description: Release notes for this CLI.
order: 99
category: Distribution
summary: User-facing release history.
---

Keep the newest release first. Describe user-visible behavior and important installation or compatibility changes rather than copying raw commit subjects.

## Unreleased

- Manage environment variables with `secret set KEY` (stdin), `secret rm KEY` and `secret list`. Add `--create` to configure a new secrets-only environment. New keys include `environment.read` and `environment.write`.
- Inspect member pins with `env status` and upgrade selected members with `env apply [MEMBER|--all]`. Status, pool, run and serve flag stale environments.
- Built-in presets skip empty checkouts. Ensure supports `requires` and honors a passing first check.
- `serve --forward auto:PORT` and `forward PORT --local auto` choose a free local port. Serve exports `BOATPARTY_FORWARD_URL`, `BOATPARTY_FORWARD_URLS` and `BOATPARTY_FORWARD_<REMOTEPORT>_URL` before expanding `[run.env]`.
- Seed gate files with `pipe --gate` and inspect them with `shell --gate`.
- Creating or resuming a member prints one notice per project per UTC day when included VM time is exhausted.

## 0.3.0 - 2026-11-26

- `run` reattaches to the original command after an SSH drop, preserving output and exit status. A disconnected client has ten minutes to return before the supervisor stops the command group.
- On Unix, `run` replaces a stale SSH control master that refuses a new session.
- Run summaries include CPU steal, start queue time, bring-up wait time and recovery. The Bun preset checks transitive packages in isolated installs.
- Cache entries expire after three days by default. Under disk pressure, boatparty evicts the least recently used entries after a run.
- Idle desktop streaming shuts down in the background. Use `desktop --open` to view the sandbox or `serve --desktop` to run a GUI application.
- Cold commands wait for file restore. Bring-up reports the current step, holder PID and failure to waiting clients.
- Direct SSH continues on degraded members. Hosting and desktop access can repair the provider tunnel with a reboot.
- Stuck members try a snapshot restart before replacement. The Rust preset invalidates affected outputs after a restart or machine move.
- `gate` checks a commit on a dedicated warm member and returns file edits as a patch.
- `[base] auto_refresh` allows a new member created from a stale snapshot to start one background base rebuild.

## 0.2.2 - 2026-11-20

- The Windows installer now offers to install missing Git and OpenSSH tools. It also offers the boat CLI for interactive sign-in. Unattended installs can opt in to required tools with `BOATPARTY_PREREQUISITES=Install`.
- The installer downloads files with `curl.exe` when available and verifies the release checksums before installing.

## 0.2.1 - 2026-11-20

- Windows x64 and Arm64 clients can now sync Git checkouts, run commands with copy-back, open interactive shells and manage boat sandboxes. Install from PowerShell with `irm https://boatparty.ashray.xyz/install.ps1 | iex`.
- Windows uses Git for Windows, the OpenSSH Client optional feature and user-specific ACLs for credentials. Each SSH operation opens its own pinned connection. Ctrl-C stops the remote command and returns 130.

## 0.2.0 - 2026-11-17

Breaking: the commands are plain verbs now. The old names are gone (no aliases):

| Before | Now |
| --- | --- |
| `cheers` | `run` |
| `cruise` | `serve` |
| `board` | `shell` (`shell -- CMD` runs one command without syncing) |
| `load` | `sync` |
| `launch` | `start` |
| `dock` | `stop` |

`pipe`, `fetch`, `forward`, `expose`, `base`, `pool` and the rest keep their names. In `--json`, the `kind` of a `shell -- CMD` result is `shell` (was `board`).

- `boatparty login --create-key` creates a boat API key with exactly the permissions boatparty needs, through the signed-in boat CLI, and stores it; the key passes through a pipe only. `doctor` now points to it. `--key-stdin` still stores a key you already have.
- The installer needs only `curl`; it no longer uses the GitHub CLI.
- The README and docs lead with the agent use case and say plainly that boatparty is for boat.dev users.

## 0.1.5 - 2026-11-15

- boatparty now uses boat's extra (paid) starts when the account has them on: once the included starts are used up it starts anyway and says so on stderr with the price, instead of refusing. `[account] paid_starts = false` (or `boatparty limits set account paid_starts=false`) turns that off. `boatparty limits` shows whether the next start is paid.
- `boatparty limits starts` shows boatparty's own start ledger: starts per rolling minute, hour and day, per project, next to boat's account totals (the gap is starts made elsewhere, such as CI or other tools), each start's time, and when the windows free up over the coming hours.

## 0.1.4 - 2026-11-15

- A member whose file restore is stuck is no longer deleted when no replacement can start today (the day's start window is full): it keeps serving commands, with one note, and is replaced at the first start that can be made.

## 0.1.3 - 2026-11-15

First published release: warm boat.dev sandbox pools for any Git project. (Tags `v0.1.0` to `v0.1.2` were cut but never published: their macOS builds exposed test portability bugs, timing races and an unsigned x86_64 binary, all fixed here.)

- `cheers` syncs the working tree as a thin Git pack (HEAD only by default), runs one command with live output and its real exit code (125 when boatparty itself failed), and applies the command's file changes back; large packs and patches travel by signed URL.
- `launch`, `load`, `board` (a shell or one command, with your Git identity, an optional GitHub token and `[run.env]`), `pipe` (a command fed standard input that is never recorded), `fetch`, `dock`, `status`, `rm`.
- `cruise` runs a server until cancelled, with port forwards and a ready port; `forward` holds a local forward and reconnects when its connection drops; `expose` publishes a gated public URL (`--hide` takes it down).
- Per-project pools with workspace affinity, one start ledger for the account, idle stop after `idle_minutes`, and every limit in `~/.config/boatparty/account.toml` (`limits`, `limits set`, `pool`, `pool prune`, `release`, `assign`).
- `boatparty.toml` with presets for Rust, Node, pnpm, Bun, Python and Go; setup and ensure steps; `[sandbox] remote_path` for the checkout's location.
- Base snapshots (`base build|status|rm`) so new sandboxes start with dependencies built; rebuilds replace the base only once the new save is ready.
- GitHub seeding: a sandbox fetches pushed commits from GitHub itself on its first sync and after large pulls; `BOATPARTY_GITHUB_TOKEN` or `BOATPARTY_GITHUB_TOKEN_COMMAND` can supply a narrower token than the `gh` login.
- Self-repair: sandboxes whose file restore stalls are replaced, build caches a restore left unreadable are dropped, and member repositories are pruned daily.
- `--json` on every command (`docs/json.md`), `doctor`, `log`, and `boatparty skill` for coding agents.
- Install with `curl -fsSL https://boatparty.ashray.xyz/install.sh | sh` (macOS and Linux, arm64 and x86_64).
