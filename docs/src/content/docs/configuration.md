---
title: boatparty.toml reference
description: Every key in boatparty.toml, the per-project configuration file, with its type, default and meaning, plus how to validate the file and see the effective settings after presets.
order: 41
category: Reference
summary: The complete schema of the checked-in project file.
---

Every project describes itself in `boatparty.toml` at the root of its repository. `boatparty init` writes a first version; you commit it, and every checkout and every machine uses the same settings.

The schema is **strict**. An unknown key or a value of the wrong type is an error that names its path, its file and the keys that are allowed, so a typo never silently does nothing:

```text
boatparty: error: /Users/you/code/app/boatparty.toml: unknown key sync.histroy (known keys in sync: history, copy_back, seed, github_token)
```

Only `version` and `[project] name` are required. Everything else has a default, comes from a [preset](/docs/presets), or is optional.

## A complete example

```toml
version = 1

[project]
name = "app"                     # lowercase letters, digits and hyphens
presets = ["rust", "pnpm"]       # applied in order; this file wins

[sandbox]
machine = "large"                # small, default, large, xlarge
desktop = "auto"                 # "keep" leaves desktop streaming running
remote_path = "/home/user/app"   # the checkout on the sandbox
environment = "app"              # a boat environment for new sandboxes

[sync]
history = "head"                 # "head" or "full"
copy_back = true
seed = true
github_token = true

[run]
ram_tmp = true
script = "pnpm run"
git_identity = true
github_token = false

[run.env]
RUST_BACKTRACE = "1"
PATH = "$HOME/.local/bin:$PATH"

[[setup]]
name = "postgres"
check = "pg_isready -q"
run = "sudo service postgresql start"

[[ensure]]
name = "web-deps"
dir = "web"
requires = ["package.json"]
key_files = ["pnpm-lock.yaml"]
run = "pnpm install --frozen-lockfile"

[build]
output_dirs = ["target", "web/.next"]
floor_mtimes = true
cache_dirs = ["incremental"]
cache_max_age_days = 3
min_free_gb = 25

[base]
warm = "cargo build --locked --all-targets"
stale_inputs = ["Cargo.lock", "web/pnpm-lock.yaml"]
ref = "origin/main"
auto_refresh = true
max_commits_behind = 50
max_age_hours = 168

[gate]
command = "cargo test --locked && pnpm -C web test"

[commands]
test = "cargo test --locked"
lint = "cargo clippy --all-targets -- -D warnings"
web = "pnpm -C web dev"
```

## Top level

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `version` | integer | yes | The file format version. Must be `1` |

## [project]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | The project's name: lowercase letters, digits and hyphens. It names the pool (`bp-<name>-1`, ...), the default checkout path and the project's entry in `account.toml`, so keep it stable |
| `presets` | list of strings | `[]` | Presets to apply, in order. Later presets and then this file override earlier ones. A name may appear only once |

## [sandbox]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `machine` | string | `account.toml`'s `[defaults] machine` | Machine size for new sandboxes: `small`, `default`, `large` or `xlarge`. Must not exceed the project's `max_machine` |
| `desktop` | string | `"auto"` | `"auto"` stops idle desktop streaming services in the background; `"keep"` leaves them running. See [Desktop](/docs/desktop) |
| `remote_path` | string | `/home/user/<name>` | The checkout's location on the sandbox. Must be a directory under `/home/user/` using letters, digits, `/`, `.`, `_` and `-`, and not inside `~/.boatparty` or `~/.cache` |
| `environment` | string | unset | A boat environment attached to new sandboxes. Unset means sandboxes get none of the account's stored secrets. See [Secrets and environments](/docs/secrets-and-environments) |

Changing `machine` or `environment` affects sandboxes created afterwards. Existing members keep what they were created with; `boatparty rm` a member to recreate it.

## [sync]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `history` | string | `"head"` | `"head"` sends only HEAD's commit (the sandbox's `git log` shows one commit). `"full"` sends every local branch and tag and the default remote branch with their history |
| `copy_back` | bool | `true` | Apply the files a `run` changes in the checkout to the local working tree |
| `seed` | bool | `true` | Let a sandbox fetch pushed commits from GitHub itself, so only what GitHub lacks is uploaded |
| `github_token` | bool | `true` | Use a GitHub token for that seed fetch (needed for private repositories) |

See [Sync and copy-back](/docs/sync-and-copy-back).

## [run]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `ram_tmp` | bool | `true` | Give each command a private, in-memory `TMPDIR`, removed when it ends |
| `script` | string | unset (the `node`, `pnpm` and `bun` presets set it) | The script runner for `boatparty run NAME` when `[commands]` has no NAME, for example `"npm run"` |
| `git_identity` | bool | `true` | Commands commit as the local `user.name` and `user.email` |
| `github_token` | bool | `false` | Git in commands and steps reaches `github.com` over HTTPS with your token, served from memory for the command's lifetime only. See [Git identity and GitHub access](/docs/github-access) |

`git_identity`, `github_token` and `[run.env]` apply to `run`, `serve`, `gate`, `pipe` and `shell`.

## [run.env]

```toml
[run.env]
RUST_BACKTRACE = "1"
DATABASE_URL = "postgres://localhost/app_test"
PATH = "$HOME/.local/bin:$PATH"
BETTER_AUTH_URL = "$BOATPARTY_FORWARD_URL"
```

Variables exported before steps and commands. Values are expanded by bash in the checkout, so `$HOME`, `$PATH` and `$PWD` (the checkout) work. A value that names its own variable (`PATH = "/x/bin:$PATH"`) **extends** the value from earlier layers instead of replacing it, so presets and your file can all add to `PATH`.

Names must be valid environment variable names, and names starting with `BOATPARTY` are reserved. Values are committed with the file, so keep secrets out; use [project secrets](/docs/secrets-and-environments) instead. During `serve`, the forward URLs (`BOATPARTY_FORWARD_URL` and friends) are set before these values are expanded.

## [[setup]]

Steps that prepare a machine, run before the first command after each boot of the sandbox's machine (a create, or a resume onto a new machine), and again when the steps themselves change.

| Key | Type | Meaning |
| --- | --- | --- |
| `name` | string | Required. Letters, digits, `-` and `_`. Layers merge steps by name |
| `check` | string | A shell command; when it succeeds, the step is skipped |
| `run` | string | Required. The shell commands to run |
| `enabled` | bool | `false` switches off a step an earlier layer (a preset) defined |

## [[ensure]]

Steps that keep dependencies current, run before a command when their inputs changed.

| Key | Type | Meaning |
| --- | --- | --- |
| `name` | string | Required. Letters, digits, `-` and `_` |
| `dir` | string | The directory to run in, relative to the checkout. `key_files` and `requires` are relative to it |
| `requires` | list of strings | Skip the whole step (including `key_command`) unless at least one of these files exists |
| `key_files` | list of strings | Files whose content forms the step's key, typically lockfiles |
| `key_command` | string | A shell command whose output is added to the key, typically a tool version (`node --version`) |
| `check` | string | A shell command; when it fails, the step runs even if the key did not change |
| `run` | string | Required. The shell commands to run |
| `enabled` | bool | `false` switches off a step an earlier layer defined |

An ensure step runs when its key changed since it last succeeded, or when its `check` fails. On the very first run, a passing `check` records the key and skips the step. See [Presets and steps](/docs/presets#steps).

## [build]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `output_dirs` | list of strings | `[]` | Directories holding build outputs, relative to the checkout (globs allowed). The `rust` preset sets `["target"]` |
| `floor_mtimes` | bool | `true` | After each command, round modification times in `output_dirs` down to whole seconds, so a restore that truncates them does not trigger rebuilds |
| `cache_dirs` | list of strings | `[]` | Names of directories under `output_dirs` holding a compiler's own caches (such as `incremental`). They are not floored, and are dropped when a restore left a file in them unreadable |
| `cache_max_age_days` | integer | `3` | After a run, remove cache entries untouched for this many days. `0` disables age-based removal |
| `min_free_gb` | integer | `25` | After a run, also remove the least recently used cache entries until the disk has this many decimal GB free. `0` disables it |

Cache cleanup looks at matching cache directories up to four levels under each output directory. During a base build, settling also removes entries untouched since the warm-up began.

## [base]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `warm` | string | from the preset | Shell commands that build everything, run by `base build` after the steps |
| `stale_inputs` | list of strings | from the preset | Files whose change makes the base stale |
| `ref` | string | the default remote branch, else HEAD | The Git ref the base is built from |
| `auto_refresh` | bool | `false` | Rebuild a stale base in the background after a new member is created from it |
| `max_commits_behind` | integer | `50` | Commits behind `ref` that make the base stale |
| `max_age_hours` | integer | `168` | Age in hours that makes the base stale |

See [Base snapshots](/docs/bases).

## [gate]

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `command` | string | unset | The shell line `boatparty gate` runs when no command is given. Read from the commit being checked |

See [Checking commits with gate](/docs/gate).

## [commands]

```toml
[commands]
test = "cargo test --locked"
"check:all" = "make check"
```

Named shell lines for `boatparty run NAME [ARGS...]`, `serve NAME` and `gate NAME`. Arguments are quoted and appended. A name starts with a letter, digit or `_`, then uses letters, digits and `- _ : . +`. A name with characters TOML does not allow in a bare key (like `:`) must be quoted.

## How layers combine

The effective configuration is built from layers, applied in order:

1. boatparty's built-in defaults;
2. each preset in `[project] presets`, in the listed order;
3. `boatparty.toml` itself, which wins.

Tables merge key by key. Other values, lists included, are replaced whole by a later layer. Two exceptions: a `[run.env]` value that names its own variable extends the earlier value, and steps merge by `name` (a later step with a known name replaces only the keys it sets, a new name is appended, and `enabled = false` switches a step off).

## Checking and inspecting the file

```bash
boatparty config validate                 # check the file and every preset it can see
boatparty config show                     # the file as parsed
boatparty config show --resolved          # the effective configuration
boatparty config show --resolved --origin # ...with where each value came from
```

`--origin` is the quickest way to answer "why is this set?":

```text
# The effective configuration of project hello-api: boatparty's defaults, then preset node (built-in), then /Users/you/code/hello-api/boatparty.toml.

version = 1  # boatparty.toml

[project]
name = "hello-api"  # boatparty.toml
presets = ["node"]  # boatparty.toml

[sandbox]
desktop = "auto"  # default
remote_path = "/home/user/hello-api"  # default

[sync]
history = "head"  # default
copy_back = true  # default
seed = true  # default
github_token = true  # default

[run]
ram_tmp = true  # default
script = "npm run"  # preset node (built-in)
git_identity = true  # default
github_token = false  # default
```

## Related files

- Presets: [Presets and steps](/docs/presets).
- Limits for each project live outside the repository, in `account.toml`: [account.toml reference](/docs/account-file).
