# boatparty

**Work here. Run afloat.** boatparty lets coding agents, and you, run a Git project's builds, tests and dev servers on warm [boat.dev](https://boat.dev) Linux sandboxes instead of the laptop. Several agents can compile, test and serve in parallel, each in its own sandbox, without melting the machine they run on.

```bash
boatparty run -- cargo test --locked
```

That one command mirrors your working tree to the sandbox, runs the command there with live output, exits with the command's real exit code, and brings any files it changed back to your checkout. The sandbox stays warm for the next command and stops by itself when you go quiet.

**Docs: [boatparty.ashray.xyz](https://boatparty.ashray.xyz)**

boatparty is for [boat.dev](https://boat.dev) users: it needs a boat account and drives boat's API. Other sandbox providers are not supported.

## Built for coding agents

- **Parallel without the heat.** Every checkout (an agent's worktree) gets its own sandbox from the project's pool, so agents run compiles, test suites and servers side by side on Linux x86_64 (Ubuntu 24.04) while the laptop stays cool.
- **Exit codes you can trust.** `125` means boatparty or the sandbox failed, never your tests; any other status is the command's own.
- **Output untouched.** stdout is the command's, byte for byte; boatparty's own messages go to stderr.
- **`--json` everywhere,** with a documented contract ([`docs/json.md`](docs/json.md)).
- **Secrets on stdin.** `boatparty pipe` feeds a command input that never reaches an argument list, a log or a file.
- **A ready-made skill.** `boatparty skill` writes a skill that teaches an agent how to use boatparty.

## Why else

- **Warm by default.** Each checkout keeps its own sandbox, with dependencies installed and build outputs kept. Only files whose content changed are synced, and when the sandbox is up, nothing talks to boat before your command starts.
- **Cheap when idle.** A sandbox stops 5 minutes after its last command and keeps its disk. Every project gets a capped pool, and one file, `~/.config/boatparty/account.toml`, holds every limit for every project, including the account's scarce sandbox starts.

## Install

macOS or Linux, with `ssh`, `ssh-keygen` and `git` on `PATH`:

```bash
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
```

This installs `boatparty` into `~/.local/bin`, checked against the release's `SHA256SUMS`. Run it again to upgrade; `| sh -s -- --version v0.2.0` installs a given release.

On Windows x64 or Arm64, install Git for Windows and the Windows OpenSSH Client optional feature. Run this in PowerShell:

```powershell
& ([scriptblock]::Create((Invoke-RestMethod https://boatparty.ashray.xyz/install.ps1)))
```

The installer verifies the ZIP against the release checksums, installs `boatparty.exe` in `%LOCALAPPDATA%\boatparty\bin`, and adds that directory to your user PATH.

## First run

With the [boat CLI](https://docs.boat.dev) installed and signed in, create and store a boat API key with exactly the permissions boatparty needs. The key passes from boat to boatparty through a pipe, so it never lands in an argument list or your shell history:

```bash
boatparty login --create-key
```

(A key you already have: `... | boatparty login --key-stdin`.)

Then, in a Git repository with at least one commit:

```bash
boatparty doctor                  # checks tools, key and scope; prints a fix for each failure
boatparty init                    # writes boatparty.toml for the detected stack
boatparty run -- cargo test       # the first run creates the sandbox, later runs reuse it warm
```

## The commands that matter

| Command | Does |
| --- | --- |
| `run` | Sync, run one command, stream its output, return its exit code, bring its changes back |
| `serve` | Run a long-lived process, such as a dev server, with port forwards |
| `shell` | Open a shell in the sandbox's checkout, or run one command there without syncing |
| `sync` | Sync the working tree only |
| `start` | Start this checkout's sandbox: create or resume it |
| `stop` | Stop the sandbox now (rarely needed: it stops by itself) |
| `status`, `limits` | This project's sandboxes; starts left, running sandboxes and usage |

Every command has `--help`. The [command reference](https://boatparty.ashray.xyz/docs/commands) covers the rest (`pipe`, `fetch`, `forward`, `expose`, `base`, `pool` and more), and [limits and pools](https://boatparty.ashray.xyz/docs/limits) explains how sandboxes are shared and stopped.

## Development

The maintainers run every check on a boat sandbox through boatparty itself, so nothing compiles on their machines; without a boat account, `just check` runs the same gate locally and CI runs it on every push. See [`CONTRIBUTORS.md`](CONTRIBUTORS.md).

```bash
just remote-check        # the full gate: fmt, clippy, tests, shellcheck, docs
just remote-test pipe    # cargo test with a filter
```

[`AGENTS.md`](AGENTS.md) has the module map and conventions, [`docs/json.md`](docs/json.md) the `--json` contract, and [`docs/boat-facts.md`](docs/boat-facts.md) the measured boat behavior the design relies on. The docs site lives in [`docs/`](docs/).
