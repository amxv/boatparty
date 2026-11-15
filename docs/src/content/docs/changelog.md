---
title: Changelog
description: Release notes for this CLI.
order: 99
category: Distribution
summary: User-facing release history.
---

Keep the newest release first. Describe user-visible behavior and important installation or compatibility changes rather than copying raw commit subjects.

## 0.1.1 - 2026-11-15

First published release: warm boat.dev sandbox pools for any Git project. (The `v0.1.0` tag was cut but never published; its macOS build exposed two test portability bugs, fixed here.)

- `cheers` syncs the working tree as a thin Git pack (HEAD only by default), runs one command with live output and its real exit code (125 when boatparty itself failed), and applies the command's file changes back; large packs and patches travel by signed URL.
- `launch`, `load`, `board` (a shell or one command, with your Git identity, an optional GitHub token and `[run.env]`), `pipe` (a command fed standard input that is never recorded), `fetch`, `dock`, `status`, `rm`.
- `cruise` runs a server until cancelled, with port forwards and a ready port; `forward` holds a local forward and reconnects when its connection drops; `expose` publishes a gated public URL (`--hide` takes it down).
- Per-project pools with workspace affinity, one start ledger for the account, idle stop after `idle_minutes`, and every limit in `~/.config/boatparty/account.toml` (`limits`, `limits set`, `pool`, `pool prune`, `release`, `assign`).
- `boatparty.toml` with presets for Rust, Node, pnpm, Bun, Python and Go; setup and ensure steps; `[sandbox] remote_path` for the checkout's location.
- Base snapshots (`base build|status|rm`) so new sandboxes start with dependencies built; rebuilds replace the base only once the new save is ready.
- GitHub seeding: a sandbox fetches pushed commits from GitHub itself on its first sync and after large pulls; `BOATPARTY_GITHUB_TOKEN` or `BOATPARTY_GITHUB_TOKEN_COMMAND` can supply a narrower token than the `gh` login.
- Self-repair: sandboxes whose file restore stalls are replaced, build caches a restore left unreadable are dropped, and member repositories are pruned daily.
- `--json` on every command (`docs/json.md`), `doctor`, `log`, and `boatparty skill` for coding agents.
- Install with `gh release download -R amxv/boatparty -p install.sh -O - | bash` (macOS and Linux, arm64 and x86_64).
