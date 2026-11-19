---
title: Overview
description: What boatparty does, the words it uses, and where its files live.
order: 1
category: Start
summary: Warm boat.dev sandbox pools for any Git project, one place per account.
---

boatparty is a command-line tool that runs a Git project's builds, tests and servers on [boat.dev](https://boat.dev) sandboxes instead of your computer. From any checkout it leases a sandbox from that project's pool, mirrors the working tree, runs a command with live output and its real exit code, brings the command's file changes back, and keeps the sandbox warm for the next command.

boatparty is built for [boat.dev](https://boat.dev) users and for coding agents: it needs a boat account, and its exit codes, untouched stdout, `--json` output and stdin-only secrets are made for agents that run builds, tests and servers in parallel. boat is the only provider it supports; the code keeps everything boat-specific behind one module (`src/provider/`), but no other backend exists.

```bash
boatparty run -- cargo test --locked
```

## What happens on a command

1. **Lease.** The checkout (a *workspace*) takes its sandbox from the project's *pool*: the one it used last, else a free one, else a new one below the project's cap. When that sandbox is already up, nothing talks to boat before the command starts.
2. **Sync.** The working tree (tracked and untracked files, minus ignored ones) travels as a thin Git pack. By default only HEAD's commit travels, not its history. Only files whose content changed are rewritten on the sandbox, so build tools see unchanged mtimes. The sandbox's repository stays bounded: its packs are rolled up as they accumulate, and once a day objects that no ref needs any more (old snapshots and results) are pruned.
3. **Run.** The command runs in the sandbox's checkout (`/home/user/<project>`, or `[sandbox] remote_path`) with the project's environment and dependency steps. stdout passes through byte for byte.
4. **Copy back.** Files the command changed in the checkout are applied to your working tree; a patch above 512 KiB comes back by signed download, which is several times faster than SSH. If the same lines changed locally meanwhile, nothing is touched and the patch is kept.
5. **Idle.** The sandbox stops by itself `idle_minutes` (default 5) after the last command ends. Its disk is kept, and the next command resumes it.

## Words

| Word | Meaning |
| --- | --- |
| Project | A named unit with its own pool and limits, named in `boatparty.toml` |
| Workspace | One checkout of a project on this machine, identified by its Git toplevel |
| Member | One sandbox in a project's pool, named `bp-<project>-<n>` at boat |
| Lease | A live claim on a member by one boatparty process (`run`, `serve` or `session`) |
| Base | An optional saved snapshot new members start from, with dependencies built |
| Preset | A TOML file of settings a project names: dependency steps, environment, build outputs |
| Runner | The bash script that is boatparty's remote half, uploaded under its content hash |

## Where things live

| Path | Content |
| --- | --- |
| `~/.config/boatparty/account.toml` | Provider settings and every limit (honors `XDG_CONFIG_HOME`) |
| `~/.config/boatparty/credentials.toml` | The boat API key, protected by a Windows ACL or Unix mode 0600 |
| `~/.config/boatparty/ssh/` | boatparty's own SSH key |
| `~/.config/boatparty/presets/` | Your presets |
| `~/.local/state/boatparty/` | `state.json`, `runs.jsonl`, SSH configs and pinned host keys per sandbox, the skill (honors `XDG_STATE_HOME`) |
| `/tmp/boatparty-<uid>/` | SSH control sockets |
| `<project>/boatparty.toml`, `<project>/.boatparty/presets/` | The project's configuration, checked in |

`BOATPARTY_HOME` replaces both roots with `$BOATPARTY_HOME/config` and `$BOATPARTY_HOME/state`.

## Sandboxes

Every sandbox is Linux x86_64 (Ubuntu 24.04) with boat's tool image: Git, compilers and runtimes for the common stacks. Sandboxes are created without the account's stored secrets unless the project names a boat environment. The API key never appears in a process argument list, a log, `state.json` or `runs.jsonl`.

A sandbox resumed onto a new machine gets its files back in the background ("hydration"). boatparty keeps such a sandbox up until that finishes, and works around files the restore leaves unreadable in build caches by dropping those caches. When a restore makes no progress, or boat reports it failing, for 10 minutes, or boat cannot reach the machine for as long, boatparty marks the member broken (`status` shows why): the next command deletes that sandbox and creates a new one under the same name, from the base snapshot when there is one. A member is a cache of your working tree, so nothing but build outputs is lost.
