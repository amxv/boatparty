---
title: Configuration and presets
description: The boatparty.toml schema, presets and how layers merge, credentials and the base snapshot.
order: 4
category: Guides
summary: One checked-in file per project, composed from plain-text presets.
---

A project describes itself in `boatparty.toml` at the repository root. The schema is strict: an unknown key is an error that names its path and file. `boatparty config validate` checks the file and every preset it can see; `boatparty config show --resolved --origin` prints the effective settings and where each came from.

```toml
version = 1

[project]
name = "app"                 # lowercase letters, digits and hyphens
presets = ["rust"]           # applied in order; this file wins

[sandbox]
machine = "default"          # small, default, large, xlarge (at most the project's max_machine)
# remote_path = "/home/user/code/app"  # the checkout; default /home/user/<name>, must be under /home/user
# environment = "ci"         # a boat environment for new sandboxes; unset: no account secrets

[sync]
history = "head"             # "head": HEAD's commit only; "full": branches, tags and history
copy_back = true             # apply the command's file changes locally
seed = true                  # fetch pushed commits on the sandbox from GitHub instead of sending them
github_token = true          # use a GitHub token for that fetch (private repositories)

[run]
ram_tmp = true               # a private RAM TMPDIR per command
script = "bun run"           # the script runner for `cheers NAME`
git_identity = true          # commands commit as your user.name and user.email
github_token = false         # git in commands reaches github.com with your token

[run.env]
RUST_BACKTRACE = "1"
PATH = "$HOME/.local/bin:$PATH"   # a value naming its own variable extends it

[[ensure]]
name = "deps"
key_files = ["Cargo.lock"]
run = "cargo fetch --locked"

[build]
output_dirs = ["target"]
floor_mtimes = true
cache_dirs = ["incremental"]

[base]
warm = "cargo test --locked --no-run"
stale_inputs = ["Cargo.lock", "rust-toolchain.toml"]

[commands]
test = "cargo test --locked"
```

## Steps

- A `[[setup]]` step (`name`, `check`, `run`) runs before the first command after each start of a sandbox, unless its `check` succeeds.
- An `[[ensure]]` step (`name`, `dir`, `key_files`, `key_command`, `check`, `run`) runs before a command when its key (the content of `key_files` plus the output of `key_command`) changed since it last succeeded, or when its `check` fails. `load` never runs steps.
- Step output goes to stderr, so stdout stays the command's. A failing step ends the run with the step's exit code and the command does not run.

## Presets

Built-in presets: `rust`, `node`, `pnpm`, `bun`, `python`, `go`. Each carries only what boat's image lacks for its stack: lockfile-keyed dependency installs, caches placed where they survive a resume, build output directories, and a warm command for the base. The `node`, `pnpm` and `bun` presets set the script runner.

Presets are plain TOML with the same shape as `boatparty.toml` minus `[project]`. They are looked up in three places, from least to most specific: built in, `~/.config/boatparty/presets/NAME.toml`, and `.boatparty/presets/NAME.toml` in the project. A file in a more specific place replaces the one below it entirely.

```bash
boatparty preset list              # every preset, where it comes from, whether this project uses it
boatparty preset show rust         # the text this project would get
boatparty preset eject rust        # copy it into .boatparty/presets/ to edit (--user: for every project)
```

Layers merge in order: defaults, each preset, then `boatparty.toml`. Tables merge by key; other values, lists included, are replaced. Steps merge by `name`: a later step with a known name replaces only the keys it sets, a new name is appended, and `enabled = false` switches a step off.

## Credentials for commands

- `[run] git_identity` (on by default) exports your local `user.name` and `user.email`, so commits made on the sandbox carry them. This and `[run] github_token` apply to `cheers`, `cruise`, `pipe` and `board`.
- `[run] github_token = true` lets git inside commands and steps reach github.com over HTTPS with `BOATPARTY_GITHUB_TOKEN` or `gh auth token`. The token is served from memory for the command's lifetime only and is never written to the sandbox. SSH-style GitHub URLs are rewritten to HTTPS. Go private modules also need `GOPRIVATE` in `[run.env]`, for example `GOPRIVATE = "github.com/your-org/*"`.
- `[sync] github_token` is separate: it lets a sync fetch a private repository from GitHub (see Seeding).

### Which GitHub token, and what it can reach

The token comes from `BOATPARTY_GITHUB_TOKEN` when set, else from the first line printed by `BOATPARTY_GITHUB_TOKEN_COMMAND` (a shell command), else from `gh auth token`. Either program runs only when a token is about to be used, and the token stays in boatparty's memory: never an argument list, a file, `state.json`, `runs.jsonl` or a log, here or on the sandbox.

By default that is your `gh` login: a long-lived OAuth token with every scope `gh` was granted (usually `repo`, so every repository you can reach, with write access). While a command runs with `[run] github_token = true`, anything the command runs on the sandbox, dependency build scripts included, can ask the credential helper for it. A seed hands it only to that one `git fetch`. GitHub offers no way to derive a shorter-lived or narrower token from a `gh` login, so to limit what a leaked token could do, give boatparty a different one:

- a fine-grained personal access token with read-only **Contents** access to just the repositories your builds fetch, and a short expiry, in `BOATPARTY_GITHUB_TOKEN`;
- or a command that mints a short-lived token, for example a GitHub App installation token (one hour, limited to the app's repositories and permissions) from a helper or your secret manager, in `BOATPARTY_GITHUB_TOKEN_COMMAND`.

Leave both `github_token` settings off for projects that only need public repositories.

## Seeding

With `[sync] seed` on and a GitHub remote, a sandbox fetches pushed commits from GitHub itself, and your machine sends only what GitHub lacks (unpushed commits and uncommitted files). That happens on a sandbox's first sync, and again whenever HEAD moved to pushed commits whose objects the sandbox lacks and those add up to 8 MiB or more (a large pull, or a switch to a distant branch; `BOATPARTY_SEED_WARM_MIN_BYTES` changes the size). Below that, sending the difference is faster than a fetch from GitHub. A sync whose HEAD did not move never looks. A failed fetch falls back to sending everything.

## The base snapshot

`boatparty base build` brings up the project's builder sandbox, syncs the base ref (`[base] ref`, by default the default remote branch, else HEAD), runs the setup and ensure steps and `[base] warm`, and saves the result as the saved snapshot `bp-<project>`. New sandboxes start from it with dependencies built, and their first sync sends only what changed since. `boatparty base status` reports its age, commits behind, changed `stale_inputs` and size. Nothing rebuilds it automatically. `boatparty base rm` deletes it and the builder.

## Getting build outputs back

Ignored files never travel in either direction. Copy an output back explicitly:

```bash
boatparty fetch target/release/app ./app-linux
```
