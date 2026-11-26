---
title: Presets and steps
description: What boatparty's built-in presets for Rust, Node, pnpm, Bun, Python and Go do, how setup and ensure steps decide when to run, and how to write, override or eject your own presets.
order: 42
category: Reference
summary: Built-in presets, where presets live, steps and how layers merge.
---

A **preset** is a small TOML file with stack-specific settings: how to install dependencies, where to keep caches so they survive a move to a new machine, which directories hold build outputs, and how to warm up a [base](/docs/bases). A project names its presets in `boatparty.toml`:

```toml
[project]
name = "app"
presets = ["rust", "pnpm"]
```

Presets keep `boatparty.toml` short, and keep boatparty itself stack-neutral: the runner knows nothing about Cargo or npm; everything specific to a language lives in a preset.

## The built-in presets

Each one carries only what boat's image lacks for its stack. The image already has the toolchains (rustup and stable Rust, Node.js 24 with npm and pnpm, Bun, Python 3.12 with pip, venv and uv, Go).

| Preset | Ensure steps | Environment and caches | Build outputs | Base warm-up |
| --- | --- | --- | --- | --- |
| `rust` | Install the toolchain from `rust-toolchain.toml`; `cargo fetch --locked` when `Cargo.lock` changes | `CARGO_HOME=/usr/local/cargo`, `RUSTUP_HOME=/usr/local/rustup` (both survive a new machine), `CARGO_NET_GIT_FETCH_WITH_CLI=true` | `target`, mtimes floored, `incremental` as a cache dir | `cargo build --all-targets` and `cargo test --no-run` |
| `node` | `npm ci` (or `npm install` without a lockfile) when the lockfile or Node.js version changes, or a package's files are missing | Script runner `npm run` | | `npm run build --if-present` |
| `pnpm` | `pnpm install --frozen-lockfile` when `pnpm-lock.yaml`, `pnpm-workspace.yaml`, Node.js or pnpm changes | Script runner `pnpm run`; `XDG_CACHE_HOME` moved somewhere that survives a new machine | | `pnpm run --if-present build` |
| `bun` | Install the Bun version `package.json` pins (checksum-verified); `bun install --frozen-lockfile` when the lockfile or Bun changes | Script runner `bun run`; `~/.bun/bin` first on `PATH` | | `bun run build` if a build script exists |
| `python` | `uv sync --frozen` with `uv.lock`; otherwise a `.venv` with `requirements*.txt` or the project installed | `VIRTUAL_ENV` and `PATH` point at `.venv`; pip and uv caches moved somewhere that survives | | Byte-compile the project |
| `go` | `go mod download` when `go.mod`, `go.sum` or the Go version changes | `GOCACHE` moved somewhere that survives a new machine | | `go build ./...` and compile tests |

Every built-in step uses `requires`, so it does nothing in a checkout that lacks its manifest (for example the `rust` steps in a repository without `Cargo.toml`). The `rust` preset also has a setup step that cleans up Cargo outputs affected by a [snapshot restart or machine move](/docs/reliability#build-outputs-after-a-restart).

Read any preset in full with:

```bash
boatparty preset show rust
```

## Where presets live

Presets are looked up in three places, from least to most specific:

| Source | Location | Use it for |
| --- | --- | --- |
| Built in | inside the boatparty binary | The defaults |
| User | `~/.config/boatparty/presets/NAME.toml` | Your own presets, or your version of a built-in one, for every project on this computer |
| Project | `.boatparty/presets/NAME.toml` in the repository | A preset the whole team shares, committed with the project |

A file in a more specific place **replaces** the one of the same name below it entirely; it does not merge with it.

```bash
boatparty preset list
```

```text
NAME    SOURCE    USED  SUMMARY
rust    built-in  no    Rust with Cargo: toolchain from rust-toolchain.toml, dependencies fetched once per Cargo.lock.
node    built-in  yes   Node.js with npm: node_modules installed once per package-lock.json, npm run as the script runner.
pnpm    built-in  no    Node.js with pnpm: node_modules installed once per pnpm-lock.yaml, pnpm run as the script runner.
bun     built-in  no    Bun: the Bun version package.json pins, node_modules installed once per bun.lock, bun run as the script runner.
python  built-in  no    Python: a .venv in the checkout, installed once per uv.lock or requirements file.
go      built-in  no    Go modules: dependencies downloaded once per go.sum, build cache kept across resumes.
```

The summary is the first comment line of each preset file.

## Customizing a preset

Most of the time you do not need to touch a preset: override the one key you care about in `boatparty.toml`, which always wins. For example, to change how the `node` preset installs dependencies:

```toml
[[ensure]]
name = "npm-install"      # the preset's step name
run = "npm ci --ignore-scripts"
```

Only `run` changes; the preset's `key_files`, `check` and `requires` stay. To switch a preset's step off entirely:

```toml
[[ensure]]
name = "cargo-fetch"
enabled = false
```

For bigger changes, **eject** the preset to get an editable copy:

```bash
boatparty preset eject rust            # into .boatparty/presets/rust.toml, for this project
boatparty preset eject rust --user     # into ~/.config/boatparty/presets/rust.toml, for every project
```

The ejected file then replaces the built-in one wherever it applies. `--force` overwrites an existing file. After editing, check it:

```bash
boatparty config validate
```

## Writing your own preset

A preset has the same shape as `boatparty.toml`, without `[project]`. Here is a preset for a project that needs a running PostgreSQL:

```toml
# Postgres: a local PostgreSQL server for integration tests.
version = 1

[run.env]
DATABASE_URL = "postgres://postgres@localhost/test"

[[setup]]
name = "postgres"
check = "pg_isready -q"
run = "sudo service postgresql start"
```

Save it as `.boatparty/presets/postgres.toml`, commit it, and add it to the project:

```toml
[project]
presets = ["node", "postgres"]
```

Keep preset files in plain text so `boatparty preset show` and code review can read them. Preset names use the same characters as project names.

## Steps

Steps prepare the sandbox before commands. Their output goes to stderr, so a command's stdout stays clean. If a step fails, the run ends with the step's exit code and the command does not run (the JSON result names the step in `failed_step`).

### Setup steps

```toml
[[setup]]
name = "redis"
check = "redis-cli ping"
run = "sudo service redis-server start"
```

A setup step runs before the first command after each **boot** of the sandbox's machine, that is after a create or a resume onto a new machine (a resume that reclaims the kept machine keeps its effects), and again when the setup steps change. If its `check` succeeds, it is skipped. Use setup steps for things that do not survive a reboot: services, mounts, kernel settings.

### Ensure steps

```toml
[[ensure]]
name = "deps"
dir = "web"
requires = ["package.json"]
key_files = ["package-lock.json"]
key_command = "node --version"
check = "test -d node_modules"
run = "npm ci"
```

An ensure step keeps something up to date with its inputs. Its **key** is the content of `key_files` plus the output of `key_command`. It runs before a command when:

- the key changed since the step last succeeded (for example, `package-lock.json` changed), or
- its `check` fails (for example, `node_modules` went missing).

On the very first run, a passing `check` records the key and skips the step, so an already-prepared checkout (such as one from a base) is not reinstalled.

`requires` makes the whole step conditional: unless at least one of the listed files exists (relative to `dir`), the step, including its `key_command`, is skipped. `sync` never runs steps; `run`, `serve` and `gate` do, and `shell` and `pipe` do not.

### How steps merge

Steps from all layers merge **by name**:

- a later layer's step with a known name replaces only the keys it sets;
- a step with a new name is appended;
- `enabled = false` switches a step off.

`boatparty config show --resolved --origin` shows the final steps and which layer set each key.

## How layers merge

1. boatparty's defaults;
2. each preset, in the order `[project] presets` lists them;
3. `boatparty.toml`.

Tables merge key by key; other values, lists included, are replaced by later layers. A `[run.env]` value that names its own variable (`PATH = "/x/bin:$PATH"`) extends the earlier value instead of replacing it. Nothing is detected at run time: `boatparty init` detects your stacks once and writes an explicit presets list.
