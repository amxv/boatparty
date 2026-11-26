---
title: Using boatparty on Windows
description: Run boatparty natively on Windows x64 and Arm64 with Git for Windows and the Windows OpenSSH Client, and learn how paths, shells, signals and connections differ from macOS and Linux.
order: 28
category: Guides
summary: What works the same, what differs, and the tools Windows needs.
---

boatparty runs natively on Windows, from PowerShell, Command Prompt or Windows Terminal. You do not need WSL. Your commands still run on the same Linux sandbox as on any other computer, so a team on mixed operating systems gets identical builds.

Install it with the PowerShell installer described in [Installation](/docs/installation#windows).

## What you need

| Tool | Why | How to get it |
| --- | --- | --- |
| Git for Windows | Snapshot and sync the working tree | The installer offers it, or [git-scm.com](https://git-scm.com/download/win) |
| Windows OpenSSH Client (`ssh.exe`, `ssh-keygen.exe`) | Reach the sandbox | The installer offers it (needs administrator approval), or Settings > Optional features |
| `tar.exe` | Only for `boatparty fetch` | Included with current Windows |
| `sh` on `PATH` | Only for `BOATPARTY_GITHUB_TOKEN_COMMAND` | Comes with Git for Windows |

Run `boatparty doctor` after installing to confirm that every tool is found.

## Remote commands use Linux syntax

The sandbox runs Linux and bash, so everything that runs there uses Linux syntax, even when you type it in PowerShell:

```powershell
boatparty run -- cargo test --locked
boatparty run --sh 'cargo test 2>&1 | tail -n 20'
boatparty fetch target/release/app .\app-linux
boatparty shell -- ls -la /home/user
```

- `--sh` lines, `[commands]` entries, `[run.env]` values and remote paths (like `/home/user/app` or `target/release/app`) are bash and Linux paths.
- Local paths, such as a `fetch` destination or `BOATPARTY_HOME`, are Windows paths.
- Quote `--sh` lines with single quotes in PowerShell, so PowerShell does not expand `$` or `|` itself.

Mirrored paths must be valid on both file systems, so avoid file names that Windows cannot represent.

## Where boatparty keeps its files

On Windows, the default locations are under your user profile:

| What | Location |
| --- | --- |
| The executable | `%LOCALAPPDATA%\boatparty\bin\boatparty.exe` |
| Configuration (`account.toml`, `credentials.toml`, SSH key, presets) | `%USERPROFILE%\.config\boatparty` |
| State (`state.json`, `runs.jsonl`, SSH configs, the skill) | `%USERPROFILE%\.local\state\boatparty` |

`BOATPARTY_HOME` accepts an absolute Windows path. The credentials and SSH keys are protected with Windows ACLs so only your user can read them.

## Connections are slower to set up

On macOS and Linux, boatparty keeps one shared SSH connection (a control master) per sandbox, and every command reuses it, so a warm command needs only one quick round trip. Windows OpenSSH does not support control sockets, so on Windows each operation opens a new, authenticated connection. Warm commands work the same way but take a little longer.

For the same reason, the `control_sockets` check in `doctor` reports `skip` on Windows.

## Cancelling

Ctrl-C in the console cancels a running command on the sandbox and returns 130, as on other systems. Windows has no Unix SIGTERM, so exit status 143 does not occur.

## Everything else is the same

Pools, leases, limits, `--json` output, exit codes, presets and `boatparty.toml` all behave exactly as described in the rest of these docs. A `boatparty.toml` written on a Mac works unchanged on Windows and vice versa.
