# boatparty

Warm [boat.dev](https://boat.dev) sandbox pools for any Git project, managed from one place per boat account.

boatparty mirrors a Git working tree to a Linux sandbox, runs a command there with live output and its real exit code, brings the command's file changes back, and keeps the sandbox warm for the next command. Every project gets its own pool of sandboxes, and one file on your machine holds the limits for the account and for each project.

```bash
boatparty cheers -- cargo test --locked     # sync, run, stream, apply changes back
```

macOS and Linux hosts. Sandboxes are Linux x86_64 (Ubuntu 24.04).

## Install

The repository is private, so installation goes through the GitHub CLI:

```bash
gh release download -R amxv/boatparty -p install.sh -O - | bash
```

It installs `boatparty` into `~/.local/bin`, checked against the release's `SHA256SUMS`. Run it again to upgrade. `ssh`, `ssh-keygen` and `git` must be on `PATH`. See [the releases page](docs/src/content/docs/releases.md) for options.

## First run

Create a scoped boat API key and store it. The key is read from standard input only, so it never appears in an argument list or shell history:

```bash
boat api-key create boatparty --ttl 90d \
  --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.delete,ssh,host,snapshot.read,snapshot.write,account.read,file.read,file.write \
  --json | jq -r .secret | boatparty login --key-stdin
```

`BOATPARTY_BOAT_API_KEY` overrides the stored key. Then, in a Git repository with at least one commit:

```bash
boatparty doctor                  # tools, account file, key validity and scope; a fix for each failure
boatparty init                    # writes boatparty.toml: project name, presets for the detected stack, a test command
boatparty cheers -- cargo test    # the first run creates the sandbox; later runs reuse it warm
boatparty cheers test             # the same through the [commands] entry init wrote
```

## The commands

The six everyday commands are boat-party themed; everything else has a plain name.

| Command | Does |
| --- | --- |
| `launch` | Start this workspace's sandbox: create or resume it, then connect |
| `cheers` | Sync, run one command in the checkout, stream its output, return its exit code, apply its file changes back |
| `load` | Sync the working tree only |
| `cruise` | Run a long-lived process, such as a dev server, until cancelled, with port forwards |
| `board` | Open a shell in the checkout, or run one command there without syncing |
| `dock` | Stop this workspace's sandbox; its disk is kept |

In a workspace: `init`, `status`, `pipe` (a command fed this process's standard input, for secrets), `fetch` (copy a file or directory back), `forward`, `expose`, `release`, `assign`, `rm`, `base build|status|rm`, `config show|validate`, `preset list|show|eject`. Anywhere: `login`, `doctor`, `limits [set]`, `pool [prune]`, `log`, `skill`. `boatparty <command> --help` documents each one.

- Exit status: the remote command's own for `cheers`, `cruise`, `pipe` and `board`; `125` when boatparty itself failed (configuration, credentials, provider, connection, limits), so a caller can tell a failing test from a failing sandbox; `130` or `143` after SIGINT or SIGTERM; `2` for usage errors.
- `--json` (before the subcommand's own arguments) prints one JSON document, or newline-delimited frames for the streaming commands. [`docs/json.md`](docs/json.md) is the contract.
- boatparty's own messages go to stderr, prefixed `boatparty: `. stdout is the remote command's, byte for byte.

## The limits model

Starts are the scarce resource: every create and every resume onto a new machine counts against the account's per-minute, per-hour and per-day windows, and boat does not queue. boatparty therefore:

- **Pools per project.** A project owns at most `max_sandboxes` sandboxes (default 8). A workspace keeps the sandbox it used last; without one it takes a free one, then a new one below the cap, then an idle one from another workspace (one whose directory is gone first, then one unused for `steal_after_minutes`), and otherwise waits up to `acquire_wait_seconds`. Nothing is deleted because a directory is missing.
- **Keeps one start ledger.** Every start passes through a local ledger shared by all projects and reconciled with boat's own counts. A start that would exceed a project's `starts_per_hour` or `starts_per_day`, or eat into the account's `reserve_starts_per_hour` and `reserve_starts_per_day`, waits with one log line; a rate-limited answer for the minute or hour window is retried after boat's delay. Only the daily window fails. `max_running` caps running sandboxes per project and for the account.
- **Stops idle sandboxes.** While a command runs, its sandbox's auto-stop is pushed `lease_minutes` ahead (at most 5) every `heartbeat_minutes`. When the last command ends, the sandbox stops `idle_minutes` later (default 5, at most 15), unless its files are still being restored after a resume. So never stop a sandbox between commands: the next command would pay a start.
- **Caps leases.** A client that dies leaves its sandbox running at most about `lease_minutes` more.

`boatparty limits` shows boat's windows, running sandboxes and included VM time, then each project's members, starts and cost this month. Limits live in `~/.config/boatparty/account.toml`; `boatparty limits set project app starts_per_hour=10` edits it without disturbing its comments.

## Files

| Path | Content |
| --- | --- |
| `~/.config/boatparty/account.toml` | Provider settings, account-wide and per-project limits. Honors `XDG_CONFIG_HOME` |
| `~/.config/boatparty/credentials.toml` | The boat API key, mode 0600 |
| `~/.config/boatparty/ssh/id_ed25519` | boatparty's own SSH key |
| `~/.config/boatparty/presets/*.toml` | Your presets |
| `~/.local/state/boatparty/` | `state.json` (pools, leases, bases, the start ledger), `runs.jsonl`, per-sandbox SSH configs, the skill. Honors `XDG_STATE_HOME` |
| `<project>/boatparty.toml`, `<project>/.boatparty/presets/` | The project's configuration, checked in |

`BOATPARTY_HOME`, when set, replaces both roots: configuration goes to `$BOATPARTY_HOME/config` and state to `$BOATPARTY_HOME/state`.

## Teaching agents

`boatparty skill` writes a skill for coding agents (a `SKILL.md` matching the binary's version) under the state directory and prints its path. It installs nothing; copy it into an agent's skills directory if you want one to use boatparty.

## Development

Nothing compiles on a developer machine: checks run on this workspace's boat sandbox through boatparty itself.

```bash
just remote-check        # the full gate: fmt, clippy, tests, shellcheck, docs
just remote-fast         # fmt and clippy
just remote-test pipe    # cargo test with a filter
just boat-build-macos --out /tmp/boatparty   # a macOS binary of this tree
```

See [`AGENTS.md`](AGENTS.md) for the module map and conventions. Measured boat behavior the design relies on is in [`docs/boat-facts.md`](docs/boat-facts.md); the docs site sources are under `docs/src/content/docs/`.
