---
title: Command reference
description: Every boatparty command with its synopsis, options, defaults and exit status, grouped by what you use it for, with links to the guides that explain each one in depth.
order: 40
category: Reference
summary: All commands and flags, matching boatparty 0.5.1.
---

This page lists every command and option in boatparty 0.5.1. `boatparty <command> --help` prints the same information in your terminal, and the guides linked from each section explain the behavior in depth.

## Global options

| Option | Meaning |
| --- | --- |
| `--json` | Print JSON instead of text: one document for most commands, newline-delimited frames for streaming ones. It is global, so it can go before or after the command name, but always before a command's own arguments (`boatparty --json run -- make`). See [JSON output](/docs/json). |
| `--help`, `-h` | Help for boatparty or for one command |
| `--version`, `-V` | Print the version |

## Overview

| Command | Does | Where it runs |
| --- | --- | --- |
| [`run`](#run) | Sync, run one command, stream it, copy its changes back | Workspace |
| [`serve`](#serve) | Run a long-lived process with port forwards | Workspace |
| [`url`](#url) | Print the local URLs of running servers | Workspace |
| [`shell`](#shell) | Open a shell, or run one command without syncing | Workspace |
| [`pipe`](#pipe) | Run one command fed from standard input | Workspace |
| [`sync`](#sync) | Sync the working tree only | Workspace |
| [`fetch`](#fetch) | Copy a file or directory back from the sandbox | Workspace |
| [`gate`](#gate) | Check a committed revision on the gate member | Workspace |
| [`start`](#start) | Create or resume this workspace's sandbox | Workspace |
| [`stop`](#stop) | Stop a sandbox, keeping its disk | Workspace |
| [`forward`](#forward) | Hold a local port forward | Workspace |
| [`expose`](#expose) | Publish a sandbox port on an HTTPS URL | Workspace |
| [`desktop`](#desktop) | Start the streamed desktop and print its link | Workspace |
| [`init`](#init) | Write `boatparty.toml` and register the project | Workspace |
| [`status`](#status) | This project's sandboxes, servers and drift | Workspace |
| [`secret`](#secret) | Manage the project's environment variables | Workspace |
| [`env`](#env) | Inspect or upgrade environment pins | Workspace |
| [`base`](#base) | Build, inspect or remove the base snapshot | Workspace |
| [`config`](#config) | Show or check the project's configuration | Workspace |
| [`preset`](#preset) | List, print or copy presets | Workspace |
| [`release`](#release) | Let go of this workspace's sandbox | Workspace |
| [`assign`](#assign) | Pin a free sandbox to this workspace | Workspace |
| [`rm`](#rm) | Delete a sandbox | Workspace |
| [`login`](#login) | Create or store the boat API key | Anywhere |
| [`doctor`](#doctor) | Check this computer and the key | Anywhere |
| [`limits`](#limits) | Show or change limits and usage | Anywhere |
| [`pool`](#pool) | Every project's sandboxes; prune unused ones | Anywhere |
| [`log`](#log) | Recent runs | Anywhere |
| [`skill`](#skill) | Write the coding-agent skill | Anywhere |

"Workspace" commands must run inside a Git checkout that has a `boatparty.toml` (except `init`, which creates it).

## Running commands

### run

```text
boatparty run [--sh LINE] [NAME [ARGS...]] [-- COMMAND...]
```

Syncs the working tree, runs one command in the sandbox's checkout, streams its output, applies its file changes back to the working tree, and exits with the command's status. Give exactly one of:

| Form | Meaning |
| --- | --- |
| `-- COMMAND...` | An argument list; every word is quoted |
| `--sh LINE` | One shell line, run by bash as given |
| `NAME [ARGS...]` | `[commands] NAME` with ARGS quoted and appended; without such an entry, `[run] script NAME ARGS`; otherwise `unknown_command`. Everything after NAME is passed on, `--` included |

Runs in one workspace take turns from snapshot through copy-back. If the same lines changed locally during the run, nothing is changed and the patch is kept under the Git directory. If SSH drops, `run` reconnects for up to ten minutes and resumes the output.

Exit status: the command's own; 125 when boatparty failed; 130 or 143 after SIGINT or SIGTERM, which also stop the remote command. Guide: [Running commands](/docs/running-commands).

### serve

```text
boatparty serve [--forward [BIND:]LOCAL:REMOTE]... [--reverse [BIND:]REMOTE:LOCAL]...
                [--ready-port PORT] [--max-minutes MINUTES] [--desktop]
                [--sh LINE] [NAME [ARGS...]] [-- COMMAND...]
```

Runs a long-lived process (a dev server) in the sandbox's checkout until cancelled. Takes the command in the same three forms as `run`, syncs first, runs the same steps with the same environment, and starts the server once the sandbox's files are fully restored. Holds a `serve` lease until it ends.

| Option | Default | Meaning |
| --- | --- | --- |
| `--forward [BIND:]LOCAL:REMOTE` | none | Make LOCAL on this computer reach REMOTE on the sandbox. Repeatable. `LOCAL` may be `auto` for a free port. Without BIND, listens on `localhost` (both `127.0.0.1` and `::1` when available) |
| `--reverse [BIND:]REMOTE:LOCAL` | none | Make REMOTE on the sandbox reach LOCAL on this computer. Repeatable. Listens on the sandbox's `127.0.0.1` without BIND |
| `--ready-port PORT` | the first `auto` forward's remote port | Log once and send the event `ready` when this sandbox port listens |
| `--max-minutes MINUTES` | 120 | Stop the server after this many minutes; `0` never stops it; fractions allowed |
| `--desktop` | off | Run in the sandbox desktop session; without a ready port, signal ready on its first visible window |
| `--sh LINE` | | Run a shell line instead of an argument list |

Local forwards export `BOATPARTY_FORWARD_URL`, `BOATPARTY_FORWARD_URLS` and `BOATPARTY_FORWARD_<REMOTEPORT>_URL` to the command before `[run.env]` is expanded. A second `serve` in the workspace claiming the same sandbox port fails with `already_serving`. Ctrl-C or SIGTERM stops the server; if `serve` dies any other way, the sandbox stops the server within seconds. Files the server changed come back only when it exits by itself.

Exit status: the server's own; 125; 130 or 143. Guide: [Dev servers, ports and URLs](/docs/dev-servers-and-urls).

### url

```text
boatparty url [REMOTEPORT]
```

Prints the local URLs of running `serve` forwards in this workspace, one per line; with REMOTEPORT, only forwards to that sandbox port. Reads local records only. Exit status: 0 when something matched, **1 when nothing matched**, 125 on a boatparty failure. Guide: [Sharing a workspace](/docs/sharing-a-workspace#finding-another-processs-server).

### shell

```text
boatparty shell [--gate] [-- COMMAND...]
```

Without a command, opens an interactive login shell in the sandbox's checkout. With `-- COMMAND...`, runs that one command (each word quoted) and exits with its status. Nothing is synced and no setup or ensure step runs; the shell or command gets `[run.env]`, the Git identity and the GitHub token when enabled. An interactive shell has no `--json` form.

| Option | Meaning |
| --- | --- |
| `--gate` | Use the project's gate member instead of the workspace's sandbox |

### pipe

```text
boatparty pipe [--gate] (--sh LINE | -- COMMAND...)
```

Runs one command in the sandbox's checkout with this process's standard input as its standard input, byte for byte. Nothing is synced first and no setup or ensure step runs; credentials and `[run.env]` apply as for `run`. The input is never written to `runs.jsonl`, `state.json` or boatparty's messages. Files the command changes stay on the sandbox. If `pipe` dies, the sandbox stops the command within seconds.

| Option | Meaning |
| --- | --- |
| `--sh LINE` | Run a shell line instead of an argument list |
| `--gate` | Use the project's gate member |

Exit status: the command's own; 125; 130 or 143.

### sync

```text
boatparty sync
```

Makes the sandbox's checkout identical to this working tree (tracked and untracked files minus ignored ones, the same HEAD and branch) without running anything. Only changed files are rewritten. Waits for an active run in the workspace and can update a running `serve`. Guide: [Sync and copy-back](/docs/sync-and-copy-back).

### fetch

```text
boatparty fetch PATH [DEST]
```

Copies a file or directory from the sandbox to this computer, without syncing. A relative PATH is resolved in the sandbox's checkout; an absolute one anywhere on the sandbox. DEST defaults to the current directory. If DEST is an existing directory, the copy keeps its name inside it; otherwise DEST is the new name. An existing file is replaced; an existing directory is never overwritten. Symbolic links are followed. Large copies travel by signed download.

### gate

```text
boatparty gate [--commit REV | --default-branch] [NAME [ARGS...] | -- COMMAND...]
```

Checks a committed revision on the project's dedicated gate member (`bp-<project>-gate`). The commit supplies the source, `boatparty.toml` and project presets. The command defaults to `[gate] command` from that commit. Edits made by the check are saved as a patch under the local Git directory, and the remote checkout is reset with ignored caches kept warm. A concurrent gate fails with `gate_busy`.

| Option | Meaning |
| --- | --- |
| `--commit REV` | The revision to check (default `HEAD`) |
| `--default-branch` | Check the locally known default remote branch, falling back to local `main` or `master` |

Exit status: the check's own; 125; 130 or 143. Guide: [Checking commits with gate](/docs/gate).

## Managing the sandbox

### start

```text
boatparty start
```

Creates or resumes this workspace's sandbox and connects, without running anything. It holds no lease afterwards, so the sandbox stops after `idle_minutes` if nothing uses it. When the account's included VM hours are used up, a start reports it once per project per UTC day.

### stop

```text
boatparty stop [MEMBER] [--wait]
```

Stops a sandbox, keeping its disk. MEMBER is a name or id; the default is this workspace's sandbox. Waits until the sandbox's files are fully restored first. Refused (`member_in_use`) while a command holds a lease. Rarely needed: sandboxes stop by themselves, and every later start counts against the account's limits.

| Option | Meaning |
| --- | --- |
| `--wait` | Also wait until the sandbox has stopped |

### release

```text
boatparty release
```

Lets go of this workspace's sandbox so another workspace may use it. The sandbox keeps running or stays stopped; only the record tying it to this workspace is dropped. Refused while a command uses it. The next command here takes a free sandbox, or the same one if it is still free.

### assign

```text
boatparty assign MEMBER
```

Pins a free sandbox of this project (name or id, from `boatparty pool`) to this workspace. It must not be in use or tied to another workspace that still exists. This workspace's previous sandbox becomes free.

### rm

```text
boatparty rm [MEMBER]
```

Deletes a sandbox at boat and forgets it. MEMBER is a name or id; the default is this workspace's sandbox. Refused while a command holds a lease. The gate member can be removed by name (`boatparty rm bp-app-gate`).

### status

```text
boatparty status
```

This project's sandboxes: state, whether files are fully restored, auto-stop time, the workspace each belongs to (`*` marks this one), running servers and their forward URLs, environment drift, and base refresh activity.

## Ports, URLs and the desktop

### forward

```text
boatparty forward REMOTE[:LOCAL] [--local PORT|auto] [--bind ADDR]
```

Forwards a local port to a sandbox port until Ctrl-C or SIGTERM, holding a `session` lease. LOCAL defaults to the same number as REMOTE. A ready line goes to stderr once it listens. When the connection drops, it is reopened (bringing the sandbox up again if needed) for up to 10 minutes.

| Option | Meaning |
| --- | --- |
| `--local PORT` | The local listening port, or `auto` for a free one |
| `--bind ADDR` | Listen on this address (an IP address, `localhost` or `*`) instead of both loopbacks |

Exit status: 130 or 143 after SIGINT or SIGTERM; 125 when the connection failed or could not be reopened.

### expose

```text
boatparty expose PORT [--public] [--localhost] [--title TITLE]
boatparty expose PORT --hide
```

Publishes a sandbox port on an HTTPS URL, printed on stdout only. The URL is stable for that sandbox and port across stops and resumes. Ports 22, 6080, 8090, 8911 and 47984 to 48010 are reserved and refused.

| Option | Meaning |
| --- | --- |
| `--public` | No access token: anyone with the URL can open it |
| `--localhost` | The service listens on the sandbox's `127.0.0.1` only (otherwise it must listen on `0.0.0.0`) |
| `--title TITLE` | A display title for the exposed port |
| `--hide` | Take the URL down; the server keeps running and the same URL returns if exposed again |

Without `--public` the URL carries an access token (`_token`); treat it like a password.

### desktop

```text
boatparty desktop [--open]
```

Starts the sandbox's streaming desktop services for this boot and prints a private, signed HTTPS link. Open it within ten minutes. The sandbox keeps its usual idle deadline. Needs the `desktop` key permission. Guide: [Desktop and GUI apps](/docs/desktop).

| Option | Meaning |
| --- | --- |
| `--open` | Open the link in the default browser instead of printing it |

## Project setup and configuration

### init

```text
boatparty init [--name NAME]
```

Writes `boatparty.toml` for this Git repository (presets for the detected stacks and a guessed `[commands] test`) and registers the project in `account.toml`. NAME defaults to the directory name and must use lowercase letters, digits and hyphens.

### config

```text
boatparty config show [--resolved] [--origin]
boatparty config validate
```

| Subcommand | Meaning |
| --- | --- |
| `show` | Print `boatparty.toml` as parsed |
| `show --resolved` | The effective configuration: defaults, then each preset in order, then `boatparty.toml` |
| `show --origin` | Annotate each value with where it came from (implies `--resolved`) |
| `validate` | Check `boatparty.toml`, the presets it names and every preset file in the user and project presets directories |

### preset

```text
boatparty preset list
boatparty preset show NAME
boatparty preset eject NAME [--user] [--force]
```

| Subcommand | Meaning |
| --- | --- |
| `list` | Every preset, where it comes from, and whether this project uses it |
| `show NAME` | The text of the preset this project would get |
| `eject NAME` | Copy a built-in preset into `.boatparty/presets/` for editing. `--user` copies it to the user presets directory instead; `--force` replaces an existing file |

Guide: [Presets and steps](/docs/presets).

### secret

```text
boatparty secret [--create] set KEY
boatparty secret [--create] rm KEY
boatparty secret list
```

Manages variables in the project's boat environment (`[sandbox] environment`). `set` reads the UTF-8 value from standard input, preserving newlines, and refuses a terminal. `list` prints names only. `--create` creates a secrets-only environment named after the project when none is configured, and sets `[sandbox] environment` in `boatparty.toml`. Needs the `environment.read` and `environment.write` key permissions. Guide: [Secrets and environments](/docs/secrets-and-environments).

### env

```text
boatparty env status
boatparty env apply [MEMBER | --all]
```

| Subcommand | Meaning |
| --- | --- |
| `status` | Each member's environment, pinned version and injected variable names, with a warning for drift from the project setting. The default pin (`base` v1) is current for an unconfigured project |
| `apply` | Upgrade old pins to the latest version: one MEMBER, `--all`, or this workspace's member. Members on another environment need recreation, which it reports |

### base

```text
boatparty base build
boatparty base status
boatparty base rm [--keep-builder]
```

| Subcommand | Meaning |
| --- | --- |
| `build` | Warm the builder sandbox (`bp-<project>-builder`) at the base ref and save it as the snapshot `bp-<project>` |
| `status` | The base's commit, age, commits behind its ref, changed stale inputs, size, refresh activity and the account's saved-snapshot allowance |
| `rm` | Delete the snapshot, its record and the builder; `--keep-builder` keeps the stopped builder so a later build is incremental |

Guide: [Base snapshots](/docs/bases).

## Account and machine

### login

```text
boatparty login --create-key [--no-verify]
boatparty login --key-stdin [--no-verify]
```

| Option | Meaning |
| --- | --- |
| `--create-key` | Create a key with every permission boatparty needs, valid 90 days, through the signed-in boat CLI (`boat api-key create`), and store it. The key passes through a pipe only |
| `--key-stdin` | Read a key you already have from standard input (refuses a terminal) |
| `--no-verify` | Store the key without asking boat whether it is valid |

The key is stored in `credentials.toml`, readable only by you. `BOATPARTY_BOAT_API_KEY` overrides it for one process.

### doctor

```text
boatparty doctor
```

Checks the tools (`ssh`, `ssh-keygen`, `git`), the account file, the API key and its permissions, each project's records against boat, and stale SSH config files and control sockets. Every failed check prints its fix. Exits 0 when all pass, 125 (`doctor_failed`) otherwise.

### limits

```text
boatparty limits
boatparty limits starts
boatparty limits set account KEY=VALUE...
boatparty limits set project NAME KEY=VALUE...
```

| Subcommand | Meaning |
| --- | --- |
| (none) | boat's start windows, running sandboxes, included VM time and machine sizes, then each project's members, running sandboxes, starts and this month's machine cost |
| `starts` | boatparty's own starts in the rolling minute, hour and day, per project, beside the account's totals; each start's time; when the windows free up |
| `set account` | Change `[account]` keys (`max_running`, `reserve_starts_per_hour`, `reserve_starts_per_day`, `paid_starts`) and `[defaults]` keys (`machine`, `max_sandboxes`, `steal_after_minutes`, `lease_minutes`, `idle_minutes`, `heartbeat_minutes`, `acquire_wait_seconds`) |
| `set project NAME` | Change `[projects.NAME]` keys (`max_sandboxes`, `max_running`, `max_machine`, `starts_per_hour`, `starts_per_day`, `idle_minutes`, `lease_minutes`) |

`limits set` keeps the file's comments and layout. An empty value (`max_running=`) removes the key. Guide: [Starts, limits and costs](/docs/limits).

### pool

```text
boatparty pool [--refresh] [--project NAME]
boatparty pool prune [--yes] [--project NAME]
```

| Option or subcommand | Meaning |
| --- | --- |
| (none) | Every project's sandboxes: state, workspace, leases, last use and environment drift |
| `--refresh` | First reconcile records with boat: adopt labelled sandboxes without a record, drop records whose sandbox is gone, delete sandboxes whose stored disk expired, free members whose workspace directory is gone |
| `--project NAME` | Only this project |
| `prune` | Delete stopped sandboxes that no existing workspace needs and that are not in use, after listing them and asking (the answer is read from standard input) |
| `prune --yes` | Delete without asking |

Guide: [Pools and leases](/docs/pools-and-leases).

### log

```text
boatparty log [-n N]
```

Recent `run`, `serve` and `pipe` invocations, oldest first. `-n`/`--limit` sets how many (default 20).

### skill

```text
boatparty skill [--print]
```

Writes the coding-agent skill (a `SKILL.md` matching this version) to `<state dir>/skill/boatparty/SKILL.md` and prints its path. It installs nothing. `--print` writes the skill to stdout instead. Guide: [Using boatparty with coding agents](/docs/coding-agents).

## Exit status

| Status | Meaning |
| --- | --- |
| The command's own | `run`, `serve`, `pipe`, `gate` and `shell -- COMMAND` exit with the remote command's status |
| 0 | Any other command succeeded |
| 1 | `url` found no matching running serve forward |
| 2 | Usage error: an unknown option or a bad argument (printed by the argument parser) |
| 125 | boatparty itself failed: configuration, credentials, provider, connection, limits. Never the command |
| 130, 143 | Cancelled by SIGINT or SIGTERM; the remote command is stopped too |

See [Exit codes and errors](/docs/exit-codes) for every error code.
