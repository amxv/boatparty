---
title: Commands
description: Every boatparty command, its plain meaning and its most useful options.
order: 3
category: Guides
summary: The everyday commands, then everything else.
---

`boatparty <command> --help` is the full reference for each command. Options such as `--json` go before a command's own arguments.

## Everyday commands

| Command | Does |
| --- | --- |
| `start` | Start this workspace's sandbox: create or resume it, then connect |
| `run` | Sync, run one command in the checkout, stream its output, apply its file changes back |
| `sync` | Sync the working tree only |
| `serve` | Run a long-lived process, such as a dev server, until cancelled |
| `shell` | Open a shell in the checkout, or run one command there without syncing |
| `stop` | Stop this workspace's sandbox; its disk is kept |

### run and serve

Both take the command in three forms:

```bash
boatparty run -- cargo test --locked          # each word after -- is quoted
boatparty run --sh 'cargo test 2>&1 | tail'   # one shell line, as given
boatparty run test -q                         # [commands] test, with -q appended
boatparty run check:rust                      # or the script runner: `bun run check:rust`
```

A name runs its `[commands]` entry; without one it runs `[run] script NAME ARGS` when the project sets a script runner, and otherwise fails with the list of known names. The `ensure` steps (dependency installs) run before the command when their inputs changed, and the `setup` steps after each start.

If SSH drops during `run`, boatparty reconnects for up to ten minutes and continues streaming the same command from its last confirmed stdout and stderr positions. Ctrl-C cancels it. If the client disappears, the sandbox stops the command group after ten minutes. If the command's record is gone, boatparty reports the loss and exits 125. Before launch, a refused SSH session can retry once, including after replacing a stale Unix control master.

`serve` holds a `serve` lease until cancelled, waits until a resumed sandbox has all its files, and carries port forwards on its own SSH connection:

```bash
boatparty serve --forward 3000:3000 --ready-port 3000 -- npm run dev
boatparty serve --reverse 20000:7777 --sh 'API=http://127.0.0.1:20000 npm start'
```

Forwards listen on 127.0.0.1 unless you name another address. `--max-minutes` (default 120) stops a forgotten server. If the client dies in any way, SIGKILL included, the sandbox stops the server within seconds.

### shell

`boatparty shell` opens a login shell in the checkout. `boatparty shell -- CMD...` runs one command without syncing and exits with its status; on a warm sandbox it costs two SSH round trips over the shared connection and no provider call, so scripts can call it often. The shell or command gets what `run` gives a command: your Git identity, the GitHub token when `[run] github_token` is on, and `[run.env]`. No setup or ensure step runs.

### stop

`stop` waits until the sandbox's files are fully restored, then stops it (`--wait` also waits for the stop). You rarely need it: sandboxes stop by themselves after `idle_minutes`, and every later start counts against the account's start limits.

## In a workspace

| Command | Does |
| --- | --- |
| `init [--name NAME]` | Write `boatparty.toml` for this repository and register the project |
| `status` | This project's sandboxes: state, files restored or not, auto-stop time, SSH config paths |
| `pipe -- CMD` / `pipe --sh LINE` | Run one command with this process's standard input, without syncing; the input is never recorded |
| `fetch PATH [DEST]` | Copy a file or directory back (relative paths are under the checkout); large ones by signed download |
| `forward REMOTE[:LOCAL]` | Hold a local port forward until cancelled; a dropped connection is reopened for up to 10 minutes, bringing the sandbox up again if needed |
| `expose PORT [--public] [--localhost] [--title T]` | A public HTTPS URL for a sandbox port, token-gated unless `--public`; `--hide` takes it down |
| `release` | Let go of this workspace's sandbox so another workspace may use it |
| `assign MEMBER` | Pin a free sandbox of the project to this workspace |
| `rm [MEMBER]` | Delete a sandbox at boat and forget it |
| `base build`, `base status`, `base rm` | Build, inspect or remove the project's base snapshot |
| `config show [--resolved] [--origin]`, `config validate` | The project's configuration as written, as resolved, or checked |
| `preset list`, `preset show NAME`, `preset eject NAME [--user]` | The presets a project can name, and copies to edit |

### pipe, for secrets

A command line ends up in `runs.jsonl` and process lists, so secrets go on standard input:

```bash
printf '%s\n' "$UPLOAD_URL" | boatparty pipe -- bash scripts/upload.sh
```

The command gets the project's credentials and `[run.env]`, and its stdout is passed through byte for byte. The input travels framed on the runner's stdin, so the end of the input is not mistaken for the end of the client: if `boatparty pipe` dies, the sandbox stops the command within seconds even while its input is still open.

## Anywhere

| Command | Does |
| --- | --- |
| `login --key-stdin [--no-verify]` | Store the boat API key, read from standard input |
| `doctor` | Check tools, account file, key and scope; match pools with boat; report stale SSH files and sockets |
| `limits`, `limits set account|project NAME KEY=VALUE...` | Show usage against every limit, or change one in `account.toml` |
| `pool [--refresh] [--project NAME]`, `pool prune [--yes]` | Every project's sandboxes; delete stopped ones no workspace needs |
| `log [-n N]` | Recent runs of `run`, `serve` and `pipe` |
| `skill [--print]` | Write the agent skill under the state directory and print its path |

## Exit status

| Status | Meaning |
| --- | --- |
| the command's own | `run`, `serve`, `pipe`, `shell -- CMD` |
| 0 | Other commands succeeded |
| 2 | Usage error |
| 125 | boatparty itself failed: configuration, credentials, provider, connection, limits. Not the command |
| 130, 143 | Cancelled by SIGINT or SIGTERM (the remote command is stopped too) |
