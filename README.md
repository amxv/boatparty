# boatparty

**Work here. Run afloat.** boatparty runs your Git project's builds, tests and dev servers on warm [boat.dev](https://boat.dev) Linux sandboxes, from any checkout, as if they ran locally.

```bash
boatparty cheers -- cargo test --locked
```

That one command mirrors your working tree to the sandbox, runs the command there with live output, exits with the command's real exit code, and brings any files it changed back to your checkout. The sandbox stays warm for the next command and stops by itself when you go quiet.

**Docs: [boatparty.ashray.xyz](https://boatparty.ashray.xyz)**

## Why

- **Your laptop stays cool.** Compiles, test suites and servers run on Linux x86_64 (Ubuntu 24.04), whatever your machine is.
- **Warm by default.** Each checkout keeps its own sandbox, with dependencies installed and build outputs kept. Only files whose content changed are synced, and when the sandbox is up, nothing talks to boat before your command starts.
- **Cheap when idle.** A sandbox stops 5 minutes after its last command and keeps its disk. Every project gets a capped pool, and one file, `~/.config/boatparty/account.toml`, holds every limit for every project, including the account's scarce sandbox starts.
- **Built for coding agents.** stdout is the command's, byte for byte; `125` means boatparty or the sandbox failed, never your tests; every command takes `--json`; secrets go in on stdin. `boatparty skill` writes a ready-made agent skill.

## Install

macOS or Linux, with `ssh`, `ssh-keygen` and `git` on `PATH`. The repository is private, so installation goes through the GitHub CLI:

```bash
gh release download -R amxv/boatparty -p install.sh -O - | bash
```

This installs `boatparty` into `~/.local/bin`, checked against the release's `SHA256SUMS`. Run it again to upgrade.

## First run

Store a scoped boat API key. It is read from standard input only, so it never lands in an argument list or your shell history:

```bash
boat api-key create boatparty --ttl 90d \
  --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.delete,ssh,host,snapshot.read,snapshot.write,account.read,file.read,file.write \
  --json | jq -r .secret | boatparty login --key-stdin
```

Then, in a Git repository with at least one commit:

```bash
boatparty doctor                  # checks tools, key and scope; prints a fix for each failure
boatparty init                    # writes boatparty.toml for the detected stack
boatparty cheers -- cargo test    # the first run creates the sandbox, later runs reuse it warm
```

## The commands that matter

| Command | Does |
| --- | --- |
| `cheers` | Sync, run one command, stream its output, return its exit code, bring its changes back |
| `cruise` | Run a long-lived process, such as a dev server, with port forwards |
| `board` | Open a shell in the sandbox's checkout, or run one command there without syncing |
| `load` | Sync the working tree only |
| `launch` | Start this checkout's sandbox: create or resume it |
| `dock` | Stop the sandbox now (rarely needed: it stops by itself) |
| `status`, `limits` | This project's sandboxes; starts left, running sandboxes and usage |

Every command has `--help`. The [command reference](https://boatparty.ashray.xyz/docs/commands) covers the rest (`pipe`, `fetch`, `forward`, `expose`, `base`, `pool` and more), and [limits and pools](https://boatparty.ashray.xyz/docs/limits) explains how sandboxes are shared and stopped.

## Development

Nothing compiles on a developer machine: checks run on this workspace's boat sandbox through boatparty itself.

```bash
just remote-check        # the full gate: fmt, clippy, tests, shellcheck, docs
just remote-test pipe    # cargo test with a filter
```

[`AGENTS.md`](AGENTS.md) has the module map and conventions, [`docs/json.md`](docs/json.md) the `--json` contract, and [`docs/boat-facts.md`](docs/boat-facts.md) the measured boat behavior the design relies on. The docs site lives in [`docs/`](docs/).
