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
- `boatparty base build` saves a snapshot with dependencies built, so new
  sandboxes start warm. `boatparty base status` says how stale it is.

## Cautions

- Do not run `rm`, `pool prune`, `base rm` or `limits set` unless the user
  asks. They delete sandboxes or change account limits.
- `serve --forward LOCAL:REMOTE` reaches a server on this computer's
  127.0.0.1. `boatparty expose PORT` gives a token-gated public URL instead
  (the server must listen on 0.0.0.0, or add `--localhost`); it is printed
  only on stdout, so treat it like a password. `expose PORT --hide` takes it
  down.
