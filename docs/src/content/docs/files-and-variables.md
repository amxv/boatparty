---
title: Files and environment variables
description: Every file boatparty reads or writes on your computer and on the sandbox, how BOATPARTY_HOME and XDG variables move them, and every environment variable boatparty reads or sets.
order: 46
category: Reference
summary: Local paths, sandbox paths, and the environment variables that change boatparty's behavior.
---

## Files on your computer

boatparty keeps two roots: a **configuration** root you may edit, and a **state** root it manages itself.

| Path | Content |
| --- | --- |
| `~/.config/boatparty/account.toml` | Provider settings and every limit, for every project. See [account.toml](/docs/account-file) |
| `~/.config/boatparty/credentials.toml` | The boat API key, readable only by you (Unix mode 0600, or a user-only ACL on Windows) |
| `~/.config/boatparty/ssh/id_ed25519` | boatparty's own SSH key pair. Your `~/.ssh` is never used or changed |
| `~/.config/boatparty/presets/` | Your [user presets](/docs/presets#where-presets-live) |
| `~/.local/state/boatparty/state.json` | Pools, members, leases, workspace affinities, bases and the start ledger. Guarded by an advisory lock, `state.lock` |
| `~/.local/state/boatparty/runs.jsonl` | One record per finished `run`, `serve` and `pipe`, read by `boatparty log`. Never holds credentials or `pipe` input |
| `~/.local/state/boatparty/ssh/` | A generated OpenSSH config and a pinned host key per sandbox (`<id>.conf`, `<id>.known_hosts`) |
| `~/.local/state/boatparty/skill/boatparty/SKILL.md` | The agent skill written by `boatparty skill` |
| `~/.local/state/boatparty/base-refresh-<project>.log` | The background base refresh log |
| `/tmp/boatparty-<uid>/` | SSH control sockets (macOS and Linux), kept short because socket paths are limited in length |

In each repository:

| Path | Content |
| --- | --- |
| `boatparty.toml` | The project's configuration, committed. See [boatparty.toml](/docs/configuration) |
| `.boatparty/presets/NAME.toml` | Project presets, committed |
| `.git/boatparty-pullback-<ms>.patch` | A command's changes that conflicted with your edits during copy-back |
| `.git/boatparty-gate-<id>.patch` | Changes a [gate](/docs/gate) check made |

boatparty does not touch your Git index, your `.git/config` or files outside the working tree. Copy-back changes only the files the command changed.

### Moving the roots

| Variable | Effect |
| --- | --- |
| `BOATPARTY_HOME` | Replaces both roots: configuration in `$BOATPARTY_HOME/config`, state in `$BOATPARTY_HOME/state`. Must be an absolute path. Useful for a separate test setup |
| `XDG_CONFIG_HOME` | Configuration in `$XDG_CONFIG_HOME/boatparty` instead of `~/.config/boatparty` |
| `XDG_STATE_HOME` | State in `$XDG_STATE_HOME/boatparty` instead of `~/.local/state/boatparty` |

On Windows, the same layout lives under your user profile (`%USERPROFILE%\.config\boatparty` and `%USERPROFILE%\.local\state\boatparty`), and `BOATPARTY_HOME` takes an absolute Windows path.

## Files on the sandbox

| Path | Content |
| --- | --- |
| `/home/user/<project>` | The checkout, or `[sandbox] remote_path` |
| `/home/user/.boatparty/bin/runner-<hash>.sh` | The runner, uploaded under its content hash |
| `/home/user/.boatparty/specs/` | The resolved project settings the runner reads, also by content hash |
| `/home/user/.boatparty/in/`, `out/` | Uploaded packs and command results in transit |
| `/home/user/.boatparty/cache/` | Caches the built-in presets move here so they survive a move to a new machine |

Locations that do **not** survive a resume onto a new machine include `~/.cache`, `/tmp` and `/var/tmp`. Your home directory and `/usr/local` do.

## Environment variables boatparty reads

| Variable | Meaning |
| --- | --- |
| `BOATPARTY_HOME` | Replaces the configuration and state roots (see above) |
| `BOATPARTY_BOAT_API_KEY` | An API key that overrides the stored one, for this process |
| `BOATPARTY_GITHUB_TOKEN` | The GitHub token to use instead of `gh auth token`. See [GitHub access](/docs/github-access) |
| `BOATPARTY_GITHUB_TOKEN_COMMAND` | A shell command whose first output line is the GitHub token, used when `BOATPARTY_GITHUB_TOKEN` is unset |
| `BOATPARTY_SEED_WARM_MIN_BYTES` | How many bytes of missing pushed commits make an existing sandbox seed from GitHub again (default 8 MiB) |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME` | Standard locations for the two roots |

The installers read their own variables: `BOATPARTY_VERSION` and `BOATPARTY_INSTALL_DIR` (both installers) and `BOATPARTY_PREREQUISITES` (Windows). See [Installation](/docs/installation).

## Environment variables boatparty sets for commands

| Variable | Set by | Value |
| --- | --- | --- |
| `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL` | `[run] git_identity` | Your local Git identity |
| `TMPDIR` | `[run] ram_tmp` | A private in-memory directory for the command |
| `BOATPARTY_FORWARD_URL` | `serve --forward` | The first local forward's URL |
| `BOATPARTY_FORWARD_URLS` | `serve --forward` | Every local forward's URL, separated by spaces |
| `BOATPARTY_FORWARD_<REMOTEPORT>_URL` | `serve --forward` | The URL for one sandbox port |
| Everything in `[run.env]` | `boatparty.toml` and presets | Your values, expanded by bash in the checkout |

Names starting with `BOATPARTY` are reserved: `[run.env]` cannot define them.
