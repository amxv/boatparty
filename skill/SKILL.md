---
name: boatparty
description: Build, test and run a Git project on a boat.dev Linux sandbox with the boatparty CLI instead of this computer. Use for compiles, test suites, dev servers and anything that needs Linux x86_64, whenever the project has a boatparty.toml or the user asks for remote builds.
---

# boatparty

This skill matches boatparty {{version}}.

boatparty mirrors this Git checkout to a warm boat.dev sandbox, runs a command
there with live output and its real exit code, and brings the command's file
changes back. Sandboxes are Linux x86_64 (Ubuntu 24.04), whatever this computer
is. `boatparty <command> --help` documents every command.

On Windows use Git for Windows and the Windows OpenSSH Client. Pass Linux paths and Bash syntax to remote commands. Ctrl-C cancels a run. Windows opens a fresh SSH connection for each operation.

## The six everyday commands

| Command | Plain meaning |
| --- | --- |
| `start` | Start this workspace's sandbox (create or resume it) |
| `run` | Sync, run one command, stream its output, apply its file changes back |
| `sync` | Sync the working tree only |
| `serve` | Run a long-lived process (a dev server) until cancelled |
| `shell` | Open a shell in the checkout, or run one command there without syncing |
| `stop` | Stop the sandbox; its disk is kept |

## The loop

```bash
boatparty init                        # once per repository: writes boatparty.toml
boatparty run -- cargo test        # every word after -- is quoted
boatparty run --sh 'make 2>&1 | tail -n 20'   # one shell line
boatparty run test                 # a [commands] entry, or the script runner
boatparty sync                        # sync without running anything
boatparty serve --forward 3000:3000 -- npm run dev   # through gg_process or similar
boatparty status                      # this project's sandboxes
boatparty limits                      # starts, running sandboxes, caps
```

- `run` exits with the command's own status. **Exit 125 means boatparty or
  the sandbox failed, not the command**: retry once, then report it as an
  infrastructure problem, never as a failing test.
- A dropped SSH session during `run` reconnects for up to ten minutes.
  Output resumes at the last received byte. If the command is reported lost,
  check for side effects before repeating it.
- stdout is the command's, byte for byte. boatparty's own messages go to
  stderr as `boatparty: ...`. `--json` (before the command) gives JSON instead.
- Edit locally; the next `run` syncs. Ignored files never leave this
  computer, and changes the command makes to tracked or untracked files come
  back. Ignored build outputs stay on the sandbox: get one with
  `boatparty fetch target/release/app [dest]`.

## Sharing a workspace between agents

- Use `boatparty url [REMOTEPORT]` to find existing serve forwards. Exit 1 means no running serve matches the workspace and optional remote port. Tool failures exit 125. `--json` includes the local URL, remote port, holder PID and start time. `status` groups servers by member.
- Run, sync and serve startup take turns in each workspace. A run holds its turn through copy-back. A waiting command reports the holder and takes its snapshot when admitted.
- Once a serve starts, other commands can run beside it. Send edits with `boatparty sync` for the dev server to reload. Keep server-generated files in ignored directories when possible.
- Reuse the URL if another serve holds the port. Stop the owning process to free it. Stop and release refuse busy members. Gate can run independently on its dedicated member.

## Starts are scarce

Every create or resume is a start, and the account has a few per minute,
hour and day. Do not `stop` between commands: a sandbox stops by itself 5
minutes after its last command ends and costs nothing while stopped. Do not
start one only to check that it works. `boatparty limits` shows what is left.
When the account's included starts are used up and boat's extra starts are
on, starts cost money (boatparty prints a `paid start` note); avoid them.

## Secrets

Never put a secret in a command line: it lands in the run log. Pass it on
standard input with `pipe`, which runs one command in the checkout without
syncing and records nothing of the input:

```bash
printf '%s\n' "$TOKEN" | boatparty pipe -- sh -c 'cat > ~/.app-token'
```

## Project settings (boatparty.toml)

- `[commands]` names lines for `run NAME [args]`; `[run] script` (for
  example `"bun run"`) runs package scripts by name: `run check:rust`.
- `presets = ["rust"]` (also node, pnpm, bun, python, go) install
  dependencies once, keyed by the lockfile. `boatparty preset show rust`,
  `boatparty preset eject rust` to edit one, `boatparty config show --resolved`
  for the effective settings, `boatparty config validate` after edits.
- `[run] git_identity` (on by default) lets commands commit as the local user.
  `[run] github_token = true` lets git on the sandbox reach private GitHub
  repositories over HTTPS with `gh auth token`, for the command's lifetime
  only. Both also apply inside `shell`, with `[run.env]`. A narrower token
  can come from `BOATPARTY_GITHUB_TOKEN` or `BOATPARTY_GITHUB_TOKEN_COMMAND`. Go private modules
  also need `GOPRIVATE` in `[run.env]`.
- `boatparty gate [--commit REV | --default-branch]` runs the committed
  `[gate] command` on the project’s dedicated member. Override it with
  `gate test` or `gate -- make check`. Gate edits come back as a patch path.
  A second concurrent gate reports `gate_busy`.
- `boatparty base build` saves a snapshot with dependencies built, so new
  sandboxes start warm. `boatparty base status` says how stale it is. Set
  `[base] auto_refresh = true` to refresh a stale base in the background
  when a new sandbox starts from it. Refreshes use the normal start ledger.

A cold `run` reports restore progress before starting the command. When another
client is bringing up the member, its PID and step appear in the wait message.
A degraded tunnel still allows direct SSH. Snapshot recovery for a stuck
member can discard remote writes made since that snapshot.

## Cautions

- Do not run `rm`, `pool prune`, `base rm` or `limits set` unless the user
  asks. They delete sandboxes or change account limits.
- `boatparty desktop --open` opens the streamed desktop. Treat its signed URL as a password. Use `serve --desktop` to start a GUI app.
- Run summaries report CPU steal. At 15% or more, host contention can cause timing-sensitive failures.
- `serve --forward LOCAL:REMOTE` publishes `http://localhost:LOCAL` on both loopbacks by default. When IPv6 is unavailable, boatparty prints a note and listens on IPv4 only. `boatparty expose PORT` gives a token-gated public URL instead
  (the server must listen on 0.0.0.0, or add `--localhost`); it is printed
  only on stdout, so treat it like a password. `expose PORT --hide` takes it
  down.

## Project environments, servers and gate files

- Manage environment variables with `secret set KEY` (stdin), `secret rm KEY` and `secret list`. Add `--create` to configure a new secrets-only environment. New keys include `environment.read` and `environment.write`.
- Inspect member pins with `env status` and upgrade selected members with `env apply [MEMBER|--all]`. Status, pool, run and serve flag stale environments.
- Built-in presets skip empty checkouts. Ensure supports `requires` and honors a passing first check.
- `serve --forward auto:PORT` and `forward PORT --local auto` choose a free local port. Serve exports `BOATPARTY_FORWARD_URL`, `BOATPARTY_FORWARD_URLS` and `BOATPARTY_FORWARD_<REMOTEPORT>_URL` before expanding `[run.env]`.
- Seed gate files with `pipe --gate` and inspect them with `shell --gate`.
- Creating or resuming a member prints one notice per project per UTC day when included VM time is exhausted.

On a warm connection the environment check uses local observations. Run `env status` after external changes. `env apply` reports which members need recreation. `requires` paths are relative to the ensure directory and any matching file enables the step.
