---
title: Getting started
description: Install boatparty, store a boat API key, and run a first command on a sandbox.
order: 2
category: Start
summary: Install, log in, doctor, init, run.
---

## Install

```bash
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
```

This installs `boatparty` into `~/.local/bin`; see [Releases and installation](/docs/releases) for options. boatparty needs `ssh`, `ssh-keygen` and `git` on `PATH`, on macOS or Linux.

On Windows x64 or Arm64, install Git for Windows and the Windows OpenSSH Client optional feature. Run this in PowerShell:

```powershell
irm https://boatparty.ashray.xyz/install.ps1 | iex
```

The installer verifies the ZIP against the release checksums, installs `boatparty.exe` in `%LOCALAPPDATA%\boatparty\bin`, and adds that directory to your user PATH.

## Store a key

boatparty talks to boat's API with a key scoped to exactly what it needs. With the [boat CLI](https://docs.boat.dev) installed and signed in, one command creates that key (valid 90 days) and stores it; the key passes from boat to boatparty through a pipe, so it never appears in an argument list or your shell history:

```bash
boatparty login --create-key
```

To store a key you already have, pipe it in: `... | boatparty login --key-stdin`. `login` checks the key with boat first (`--no-verify` skips that). `BOATPARTY_BOAT_API_KEY` overrides the stored key for one process.

## Check the machine

```bash
boatparty doctor
```

`doctor` checks the tools, the account file, the key and its scope, matches every project's records with boat, and reports stale SSH files and control sockets. Every failed check prints the command that fixes it.

## Set up a project

In a Git repository with at least one commit:

```bash
boatparty init
```

`init` writes `boatparty.toml` with the project name (from the directory, or `--name`), the presets for the stacks it detects (Rust, Node, pnpm, Bun, Python, Go) and a guessed `[commands]` entry, and registers the project in `account.toml`. Commit the file.

## Run something

```bash
boatparty run -- cargo test --locked    # every word after -- is quoted
boatparty run --sh 'make 2>&1 | tail'   # one shell line
boatparty run test                      # a [commands] entry
```

The first command creates the sandbox and installs dependencies; later ones reuse both. `run` exits with the command's status, or 125 when boatparty itself failed. Then:

```bash
boatparty status     # this project's sandboxes
boatparty limits     # starts left, running sandboxes, per-project usage
boatparty log        # recent runs
```

Leave the sandbox alone between commands: it stops by itself five minutes after the last one, and stopping it by hand only costs a start on the next command.

## Teach an agent

```bash
boatparty skill
```

writes a skill for coding agents under the state directory and prints its path. Copy it into an agent's skills directory if you want an agent to use boatparty.
