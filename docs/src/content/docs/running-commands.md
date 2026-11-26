---
title: Running commands
description: Everything about boatparty run, named commands and script runners, dependency steps, exit codes and cancelling, plus shell, pipe, fetch and log for the jobs run does not cover.
order: 20
category: Guides
summary: run in its three forms, named commands, shell, pipe, fetch and the run log.
---

`boatparty run` is the command you will use most. It syncs your working tree, runs one command on the sandbox, streams its output, exits with its exit code and brings its file changes back. This guide covers `run` in depth, then the commands for jobs that `run` is not meant for.

## Three ways to give a command

### An argument list, after `--`

```bash
boatparty run -- cargo test --locked
boatparty run -- python3 -m pytest -k "parser and not slow"
```

Everything after `--` is the command and its arguments. Each word is quoted for you, exactly as your local shell passed it, so spaces and special characters arrive intact. This is the safest form and the one to use by default.

### A shell line, with `--sh`

```bash
boatparty run --sh 'cargo test 2>&1 | tail -n 20'
boatparty run --sh 'cd web && npm ci && npm run build'
```

`--sh LINE` runs one line of bash on the sandbox, as written. Use it when you need pipes, redirects, `&&`, globs or environment variable expansion on the sandbox. Put the line in single quotes so your local shell does not expand anything first.

### A name

```bash
boatparty run test
boatparty run test -q
boatparty run check:rust
```

A name runs a command you defined in `boatparty.toml`:

```toml
[commands]
test = "cargo test --locked"
lint = "cargo clippy --all-targets -- -D warnings"
e2e = "npm run test:e2e --"
```

Any words after the name are quoted and appended, so `boatparty run test -q` runs `cargo test --locked -q`. Everything after the name is passed on, including `--`: `boatparty run test -- --nocapture` runs `cargo test --locked -- --nocapture`.

If `[commands]` has no entry for the name, boatparty uses the project's **script runner** when it has one. The `node`, `pnpm` and `bun` presets set it to `npm run`, `pnpm run` and `bun run`, so every `package.json` script works by name:

```bash
boatparty run build          # npm run build
boatparty run check:rust     # bun run check:rust, in a Bun project
```

You can set the script runner yourself with `[run] script = "make"` or similar. If neither a command nor a script runner matches, `run` fails with `unknown_command` and lists the names it knows.

Options such as `--json` go before the name: `boatparty --json run test`.

## What the command gets

The command runs in the sandbox's checkout, `/home/user/<project>` (or `[sandbox] remote_path`), with:

- **The project's environment.** Variables from `[run.env]` in `boatparty.toml` and its presets, for example `CARGO_HOME` from the `rust` preset or `VIRTUAL_ENV` from `python`.
- **Your Git identity.** `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL` and the committer equivalents come from your local `user.name` and `user.email`, so commits made on the sandbox are yours. Turn this off with `[run] git_identity = false`.
- **Optionally, a GitHub token** for private dependencies, with `[run] github_token = true`. See [Git identity and GitHub access](/docs/github-access).
- **A private RAM temporary directory.** With `[run] ram_tmp = true` (the default), `TMPDIR` points at a fresh directory in memory for each command, which keeps scratch files fast and off the disk. It is removed when the command ends.
- **No standard input.** `run` does not forward your terminal's input, so interactive prompts see end-of-file. Use [`pipe`](#pipe-send-input-to-a-command) to feed input, or [`shell`](#shell-a-terminal-on-the-sandbox) for an interactive session.

## Dependency steps

Before your command, the runner checks the project's steps (most come from [presets](/docs/presets)):

- **Ensure steps** install dependencies when their inputs change. The `node` preset's `npm-install` step runs `npm ci` when `package-lock.json` or the Node.js version changes, or when `node_modules` is missing or incomplete.
- **Setup steps** run once per boot of the sandbox's machine, for things like starting a service.

You see them on stderr:

```text
boatparty: ensure npm-install (first run)
...
boatparty: ensure npm-install done in 1s
```

If a step fails, its output and exit code are reported, the run ends with the step's exit code, and your command does not run. With `--json`, the result's `failed_step` names the step.

## Exit codes and cancelling

| Exit status | Meaning |
| --- | --- |
| The command's own | The command ran and exited with this status |
| 125 | boatparty itself failed (configuration, key, connection, sandbox, limits); the command's result is unknown |
| 130 | You pressed Ctrl-C (SIGINT). The remote command was stopped too |
| 143 | boatparty received SIGTERM. The remote command was stopped too |

Pressing Ctrl-C sends the cancellation to the sandbox, which stops the command's whole process group, so no stray build keeps running. If your connection drops instead, the command keeps running and boatparty reconnects; see [Reliability and recovery](/docs/reliability).

After each run boatparty prints a one-line summary on stderr:

```text
boatparty: bp-hello-api-1 (bx_d5zd8s2j) exit 0 in 1.4s (prep 1.1s, 0% CPU steal)
```

It names the sandbox, the exit code, the total time, the time spent on sync and steps (`prep`), and the host's CPU steal. On a cold start it also says `started the sandbox`, and it notes time spent queued for a start or recovering a sandbox.

## When several runs share a workspace

Two `run` commands in the same checkout take turns: the second waits until the first has finished and copied its changes back, then syncs and runs. That way the second run always sees a consistent tree. A dev server started with `serve` does not block runs once it is up. See [Sharing a workspace between agents](/docs/sharing-a-workspace).

## shell: a terminal on the sandbox

```bash
boatparty shell
```

opens an interactive login shell in the sandbox's checkout. It has the same environment a `run` command gets: `[run.env]`, your Git identity and the GitHub token if enabled. No setup or ensure step runs, and nothing is synced first; run `boatparty sync` first if you want your latest edits there.

With a command after `--`, `shell` runs just that command and exits with its status:

```bash
boatparty shell -- uname -sm
```

```text
Linux x86_64
```

This is the lightest way to ask the sandbox something. On a warm sandbox it costs two SSH round trips over the shared connection and no call to boat's API, so scripts and agents can call it often. Each word after `--` is quoted, so `boatparty shell -- sh -c 'ls | wc -l'` runs exactly that.

`shell --gate` opens the shell on the project's [gate member](/docs/gate) instead.

## pipe: send input to a command

```bash
printf '%s\n' "$UPLOAD_URL" | boatparty pipe -- bash scripts/upload.sh
boatparty pipe --sh 'cat > ~/.config/app/token && chmod 600 ~/.config/app/token' < token.txt
```

`pipe` runs one command (after `--`, or a `--sh` line) with boatparty's own standard input as the command's standard input, byte for byte. It exists for **secrets and other sensitive input**: the input is never written to `runs.jsonl`, `state.json` or any boatparty message, while a command line would be recorded in all of those.

Like `shell -- CMD`, `pipe` does not sync first and runs no setup or ensure step, but the command gets `[run.env]`, your Git identity and the GitHub token if enabled. Files it changes stay on the sandbox. The summary reports how many bytes it read, never what they were:

```text
boatparty: bp-hello-api-1 (bx_d5zd8s2j) pipe exit 0 in 0.7s (13 input bytes)
```

If `boatparty pipe` dies for any reason (even SIGKILL), the sandbox stops the command within seconds, even if its input was still open. `pipe --gate` runs on the gate member, which is handy to seed a fixture file before a gate.

## fetch: bring a file back

Ignored build outputs never come back on their own. `fetch` copies a file or directory from the sandbox:

```bash
boatparty fetch target/release/app                 # into the current directory
boatparty fetch target/release/app /tmp/app-linux  # under a new name
boatparty fetch /tmp/report out/                   # an absolute path, into out/
```

A relative path is resolved in the sandbox's checkout. Nothing is synced first. If the destination is an existing directory, the copy keeps its name inside it; otherwise the destination is the new name. An existing file is replaced; an existing directory is never overwritten. Symbolic links are followed. Large copies travel through a signed download URL, which is much faster than SSH.

## log: what ran recently

```bash
boatparty log -n 5
```

```text
2026-11-26T18:30:53Z  exit 3         1.3s (prep 1.2s)  hello-api bp-hello-api-1  node -e 'process.exit(3)'
2026-11-26T18:31:26Z  exit 0         1.4s (prep 1.1s)  hello-api bp-hello-api-1  npm run build
2026-11-26T18:31:29Z  exit 0         0.7s  hello-api bp-hello-api-1  wc -c
2026-11-26T18:31:56Z  exit 1         1.4s (prep 1.1s)  hello-api bp-hello-api-1  npm test
2026-11-26T18:31:30Z  cancelled (143) 28.6s (prep 6.1s)  hello-api bp-hello-api-1  npm run dev
```

`log` lists recent `run`, `serve` and `pipe` invocations from every project, oldest first (20 by default, `-n N` for more or fewer). Each line has the start time, the outcome, the duration, the project, the sandbox and the command. The records live in `runs.jsonl` in boatparty's state directory and never contain credentials or `pipe` input. Note the `pipe` line shows the command (`wc -c`), never its input.

## start, sync and stop

These are rarely needed, because `run` does all three as required:

```bash
boatparty start         # create or resume this workspace's sandbox now
boatparty sync          # mirror the working tree without running anything
boatparty stop          # stop the sandbox now, keeping its disk
```

- `start` is useful to warm a sandbox up before you need it, for example at the start of a session. It prints the auto-stop time; the sandbox stops after `idle_minutes` if nothing uses it.
- `sync` is mainly for updating a running dev server; see [Dev servers](/docs/dev-servers-and-urls#send-edits-to-a-running-server).
- `stop` waits until the sandbox's files are fully restored, then stops it (`--wait` also waits until boat reports it stopped). It refuses while a command is using the sandbox. Remember that the next command will need a start; letting the sandbox stop by itself is almost always better.

```text
$ boatparty start
boatparty: resuming bp-hello-api-1 (bx_d5zd8s2j)
bp-hello-api-1 (bx_d5zd8s2j) resumed on its kept machine; auto-stop 2026-11-26T18:37:38.341Z
enter it with `boatparty shell`
```
