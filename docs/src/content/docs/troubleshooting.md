---
title: Troubleshooting
description: Fixes for common boatparty problems, from exit code 125 and API key errors to start limits, busy sandboxes, slow first runs, copy-back conflicts, missing files on the sandbox and dev server URLs.
order: 29
category: Guides
summary: Symptoms, causes and fixes, starting with doctor and exit code 125.
---

Start here when something does not work. Most problems fall into a few groups: setup (key, tools, project file), limits (starts, pool, running cap), and the difference between your computer and a Linux sandbox.

## First steps

1. **Read the message.** boatparty's own messages go to stderr and start with `boatparty: `. Errors start with `boatparty: error: ` and usually say what to do next.
2. **Check the exit status.** If it is not 125, boatparty worked and the *command* failed; debug it as you would locally. If it is 125, boatparty or the sandbox failed. See [Exit codes and errors](/docs/exit-codes).
3. **Run `boatparty doctor`.** It checks tools, the key and its permissions, the account file and the pools, and prints a fix for every failure.
4. **Retry once.** Many 125 failures are transient (a network hiccup, a provider timeout). If the retry fails the same way, use the sections below.
5. **Use `--json` for details.** `boatparty --json run ...` puts a stable error `code` in the result, which the table in [Exit codes and errors](/docs/exit-codes) explains.

## Setup problems

### "no API key"

```text
FAIL  credentials  no API key: /Users/you/.config/boatparty/credentials.toml does not exist and BOATPARTY_BOAT_API_KEY is not set
                   fix: boatparty login --create-key
```

You have not stored a key yet (or `BOATPARTY_HOME` points somewhere else). Run `boatparty login --create-key` with the boat CLI signed in, or pipe a key into `boatparty login --key-stdin`. See [Your first run](/docs/getting-started#1-store-a-boat-api-key).

### The key is rejected, expired or missing permissions

- `the provider rejects the API key` or `the API key has expired`: keys from `login --create-key` expire after 90 days. Run `boatparty login --create-key` again.
- `key_scope` fails with `the key lacks: ...`: a newer boatparty version uses a permission your key predates (for example, 0.4.0 added `environment.read` and `environment.write`). Run `boatparty login --create-key` again.
- `the key is limited to particular sandboxes or environments`: boatparty creates its own sandboxes, so it needs a key that is not restricted to specific ones. Create a new key with `login --create-key`.

Error codes: `unauthorized`, `key_scope_forbidden`, `credentials`.

### "does not exist; run `boatparty init`"

The repository has no `boatparty.toml`. Run `boatparty init` at the repository root and commit the file. In a fresh worktree of a repository that has the file committed, it is already there.

### "not inside a Git working tree" or "no commit yet"

boatparty mirrors Git working trees. Run it inside a Git repository that has at least one commit (`git commit --allow-empty -m init` is enough for a brand-new one). Error code: `workspace`.

### An unknown key in boatparty.toml or account.toml

Both files are strict: a misspelled key is an error that names its path and lists the known keys, for example `unknown key sync.histroy (known keys in sync: history, copy_back, seed, github_token)`. Fix the spelling. `boatparty config validate` checks `boatparty.toml` and every preset it can see.

### ssh, ssh-keygen or git is not on PATH

Install the missing tool. On Windows, rerun the installer with `$env:BOATPARTY_PREREQUISITES = 'Install'`, or see [Using boatparty on Windows](/docs/windows).

### doctor warns about stale SSH files or sockets

`ssh_configs` and `control_sockets` warnings list leftovers from sandboxes boatparty no longer knows, or SSH connections that died without cleaning up. They are harmless; the printed fix removes them.

## Limits and waiting

### "waits for ...": a start is queued

```text
boatparty: starting bp-app-2 waits for the account's hourly starts (60 of 60 used): up to 14m 05s
```

Your account (or the project's `starts_per_hour`) has used its starts for the current minute or hour, so boatparty waits for the window to roll over instead of failing. This is normal under heavy use. To avoid it: let sandboxes stop by themselves instead of stopping them, avoid deleting and recreating members, and check `boatparty limits starts` to see what used the starts. See [Starts, limits and costs](/docs/limits).

### daily_start_limit

The day's start window is full, at boat or in the project's `starts_per_day`. boatparty fails at once rather than waiting for hours. Warm, running sandboxes still work (they need no start). `boatparty limits starts` shows when starts leave the window. If your plan has paid extra starts, boatparty can use them unless `paid_starts = false`.

### pool_exhausted

Every member of the pool was in use for `acquire_wait_seconds` (15 minutes by default) and the pool was at `max_sandboxes`. Either too many workspaces are busy at once, or something holds leases for a long time (a forgotten `serve` or `forward`). Check `boatparty pool`, stop what you do not need, or raise the cap with `boatparty limits set project NAME max_sandboxes=10`.

### running_limit

`max_running` (the account's or the project's) stayed reached for the whole wait, so no sandbox could start. Check what is running with `boatparty pool`; idle sandboxes stop after `idle_minutes`. Raise the limit with `limits set` if it is too low for your workload.

### member_in_use: stop, release or rm refused

```text
boatparty: error: cannot stop bp-app-1: it is in use by pid 72373 (npm run dev); wait for those commands to finish
```

A process (here a `serve`) holds a lease on the sandbox. Stop that process first, or leave the sandbox alone; it stops by itself when idle.

### "waiting for run (pid ...)"

Another `run` in the same workspace is in progress, and runs take turns. This is expected; see [Sharing a workspace](/docs/sharing-a-workspace). If the other process is stuck, stop it.

## Slow or stuck sandboxes

### The first command on a sandbox is slow

A new member has to receive the project, install dependencies and build from scratch. Later commands are incremental. If new members are frequent (many worktrees, replacements), build a [base snapshot](/docs/bases) so they start warm.

### "restoring: ... entries"

The sandbox resumed onto a new machine and its disk is being restored. boatparty waits for it to finish before syncing, then continues. If the restore stops progressing for 10 minutes, boatparty recovers the sandbox automatically. See [Reliability and recovery](/docs/reliability).

### A sandbox was restarted or replaced

```text
boatparty: bp-app-1 is stuck (restore stalled); force-stopping to resume from the last snapshot. Writes since that snapshot will be lost
```

boatparty's recovery ladder fixed a stuck sandbox. Your working tree is safe (it is on your computer); files written only on the sandbox since the last snapshot may be gone. Rerun the command.

### Tests are slower or flakier than locally

Look at the CPU steal in the run summary. At 15% or more the host was overloaded, and timing-sensitive tests can fail for that reason. Retry later, or use a larger machine size (`[sandbox] machine = "large"`, within the project's `max_machine`).

## Files and sync

### A file is missing on the sandbox

Ignored files never travel. If your tests need `.env`, a local config file or generated assets that `.gitignore` excludes, they are not on the sandbox. Options:

- put non-secret settings in `[run.env]`;
- put secrets in a [project environment](/docs/secrets-and-environments) with `boatparty secret set`;
- generate the file on the sandbox, in a setup or ensure step, or as part of the command;
- write a one-off file with `boatparty pipe --sh 'cat > path' < file`.

### A build output did not come back

Build outputs are usually in ignored directories, which copy-back skips by design. Use `boatparty fetch PATH`. See [Sync and copy-back](/docs/sync-and-copy-back#getting-build-outputs-back).

### CONFLICT: the command changed a file you also changed

```text
boatparty: CONFLICT: the command changed greet.js but the local copy changed too; nothing was changed locally and the patch is kept at /Users/you/code/app/.git/boatparty-pullback-1795717954926.patch
```

You edited the same lines while the command ran. Nothing was overwritten. Review the patch with `git apply --stat PATH` and merge it with `git apply --3way PATH` if you want it. See [Sync and copy-back](/docs/sync-and-copy-back#when-a-change-conflicts-with-your-edits).

### unknown_command

`boatparty run NAME` found no `[commands]` entry called NAME and the project has no script runner. The error lists the names it knows. Add the command to `[commands]`, or use `boatparty run -- ...` instead.

### A command waits for input forever, or gets end-of-file

`run` does not forward your terminal's input. Pass input with `boatparty pipe`, or use `boatparty shell` for interactive work, or make the command non-interactive (`--yes`, `CI=1`).

## Servers and URLs

### boatparty url exits 1

No `serve` with a forward is running in this workspace (or none for that port). `url` only knows servers started with `serve --forward` from the same worktree. Standalone `forward` tunnels are not listed. Start the server, or check you are in the right directory.

### already_serving

Another `serve` in this workspace already serves that sandbox port. Use its URL (`boatparty url PORT`), or stop it first.

### port_in_use

The local port you asked for is taken on your computer. Use `--forward auto:PORT` (or `forward PORT --local auto`) to get a free one.

### Auth or CORS fails on localhost after upgrading to 0.5.1

Since 0.5.1, forwards publish `http://localhost:PORT` instead of `http://127.0.0.1:PORT`. If your app's CORS rules, OAuth redirect URIs or auth configuration name `127.0.0.1`, add the `localhost` origin, use `$BOATPARTY_FORWARD_URL` in `[run.env]`, or pass an explicit bind address (`--forward 127.0.0.1:3000:3000`) to keep the old URL.

### The server is unreachable from another computer

Local forwards listen on your loopback addresses only. Use `--forward 0.0.0.0:3000:3000` to listen on all interfaces, or `boatparty expose 3000` for a public HTTPS URL.

### expose: the URL shows an error page

The service must listen on `0.0.0.0`. For a service bound to `127.0.0.1` on the sandbox, add `--localhost`. Webhook senders usually need `--public`.

## Environment warnings

### "env: app v1 (expected app v2); see boatparty env apply"

A member is pinned to an older version of the project's environment, so it does not have your latest secrets. Run `boatparty env apply --all`. When the project has no configured environment, a `base` v1 pin is current and requires no action. See [Secrets and environments](/docs/secrets-and-environments#environment-versions-and-pins).

## Still stuck?

- `boatparty status` and `boatparty pool --refresh` show the current state of each sandbox, reconciled with boat.
- `boatparty log` shows recent runs and their outcomes.
- [Open an issue](https://github.com/amxv/boatparty/issues) with the command, its full stderr and the `boatparty --version` output. Never include your API key, tokens or `expose`/`desktop` URLs.
