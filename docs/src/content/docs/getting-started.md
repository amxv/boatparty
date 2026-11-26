---
title: Your first run
description: Create a scoped boat API key, check your machine with doctor, set up a project with init, and run your first command on a warm sandbox.
order: 3
category: Start
summary: Log in, doctor, init, run. About five minutes from install to green tests.
---

This guide takes you from a fresh install to your first command running on a sandbox. It uses a small Node.js project called `hello-api` as the example, but every step is the same for Rust, Python, Go, Bun or anything else. If you have not installed boatparty yet, start with [Installation](/docs/installation).

## 1. Store a boat API key

boatparty talks to boat's API with an API key. The safest way to get one is to let boatparty create it through the boat CLI:

```bash
boatparty login --create-key
```

```text
boatparty: stored the API key in /Users/you/.config/boatparty/credentials.toml (mode 0600); the provider accepted it
boatparty: run `boatparty doctor` to check that the key allows everything boatparty does
```

Behind the scenes this runs `boat api-key create` for you and asks for a key that:

- has **exactly the permissions boatparty uses** (creating, resuming, stopping and deleting sandboxes, SSH, hosting, the desktop, snapshots, account limits, environments and signed file transfers), and nothing more;
- is named `boatparty` and **expires after 90 days**.

The key travels from the boat CLI to boatparty through a pipe. It never appears in a command line, your shell history or a log. boatparty then asks boat whether the key works before saving it, and stores it in `credentials.toml`, readable only by you (Unix mode 0600, or a user-only ACL on Windows).

For this to work, the [boat CLI](https://docs.boat.dev) must be installed and signed in (`boat login`).

### Using a key you already have

If you created a key yourself, pipe it in. Avoid pasting the key into a command line; read it from a file or a password manager instead:

```bash
cat ~/boat-key.txt | boatparty login --key-stdin
op read "op://Private/boat/api-key" | boatparty login --key-stdin
```

`login` checks the key with boat before storing it. Add `--no-verify` to skip that check (for example when you are offline). A key that boat rejects, or one that has expired, is never stored.

### Using an environment variable instead

When `BOATPARTY_BOAT_API_KEY` is set, boatparty uses it instead of the stored key, for that one process. This is useful in CI or for a one-off test with a different key. Prefer the stored key for everyday use, because environment variables are easier to leak.

## 2. Check your machine

```bash
boatparty doctor
```

On a healthy machine every line says `ok`:

```text
ok    ssh          /usr/bin/ssh
ok    ssh-keygen   /usr/bin/ssh-keygen
ok    git          /usr/bin/git
ok    account      /Users/you/.config/boatparty/account.toml
ok    credentials  API key in /Users/you/.config/boatparty/credentials.toml (mode 0600)
ok    key          the provider accepts the API key
ok    key_scope    allows all 16 permissions boatparty uses
ok    pools        no projects yet
ok    ssh_configs  0 files in /Users/you/.local/state/boatparty/ssh, none stale
ok    control_sockets 2 sockets in /tmp/boatparty-501, none stale
```

`doctor` checks the local tools, the account file (creating it with defaults the first time), the API key and whether it has every permission boatparty needs, then compares each project's records with what boat reports. Every failed check prints the command that fixes it. For example, before you log in:

```text
FAIL  credentials  no API key: /Users/you/.config/boatparty/credentials.toml does not exist and BOATPARTY_BOAT_API_KEY is not set
                   fix: boatparty login --create-key
skip  key          needs a readable account file and a key
boatparty: 1 check failed
```

`doctor` exits 0 when every check passes and 125 when any check fails, so you can use it in scripts.

## 3. Set up the project

Go to your repository. It must be a Git repository with at least one commit.

```bash
cd ~/code/hello-api
boatparty init
```

```text
wrote /Users/you/code/hello-api/boatparty.toml for project hello-api
presets: node (from package.json)
command: `boatparty run test` runs `npm test`
registered project hello-api in /Users/you/.config/boatparty/account.toml
project hello-api has no base, so each new sandbox builds its dependencies first; `boatparty base build` saves one
next: `boatparty start` starts this workspace's sandbox
```

`init` did three things:

1. **Detected your stack** from the files in the repository and chose matching [presets](/docs/presets): `rust` (from `Cargo.toml`), `node`, `pnpm` or `bun` (from `package.json`, choosing by lockfile or the `packageManager` field), `python` (from `pyproject.toml`, `requirements.txt`, `setup.py` or `uv.lock`) and `go` (from `go.mod`). A repository with several stacks gets several presets.
2. **Wrote `boatparty.toml`**, with the project name (the directory name, or `--name NAME`), the presets, and a guessed `[commands]` entry for running tests (`cargo test --locked`, `npm test`, `pnpm test`, `bun run test`, `.venv/bin/python -m pytest` or `go test ./...`).
3. **Registered the project** in your account file, so it gets its own sandbox pool and limits.

The generated file is short and commented:

```toml
# boatparty project configuration. Check this file in.
version = 1

[project]
name = "hello-api"
presets = ["node"]

[commands]
test = "npm test"
```

Commit it, so every checkout and every teammate's machine uses the same settings:

```bash
git add boatparty.toml
git commit -m "Add boatparty.toml"
```

The [boatparty.toml reference](/docs/configuration) lists everything you can add later.

## 4. Run your first command

```bash
boatparty run test
```

`test` is the `[commands]` entry `init` wrote, so this runs `npm test` on the sandbox. The first time, boatparty has to create the sandbox and install dependencies, so it takes a few seconds longer:

```text
boatparty: creating bp-hello-api-1 (default machine)
boatparty: ensure npm-install (first run)

up to date, audited 1 package in 167ms

found 0 vulnerabilities
boatparty: ensure npm-install done in 1s

> hello-api@1.0.0 test
> node --test

✔ greets by name (0.445502ms)
✔ greets the crew (0.107676ms)
ℹ tests 2
ℹ pass 2
ℹ fail 0
boatparty: applied the command's changes locally: package-lock.json
boatparty: bp-hello-api-1 (bx_d5zd8s2j) exit 0 in 8.7s (prep 8.1s, 0% CPU steal, started the sandbox)
```

Here is what happened, line by line:

- **`creating bp-hello-api-1`**: the project's pool was empty, so boatparty created its first sandbox (a *member* of the pool).
- **`ensure npm-install (first run)`**: the `node` preset's dependency step ran, because the sandbox had never installed this project's packages.
- **The test output** is exactly what `npm test` printed, streamed live.
- **`applied the command's changes locally: package-lock.json`**: `npm install` created a lockfile on the sandbox, and boatparty copied it back into your working tree. You will find it as a new untracked file.
- **The summary line** names the sandbox, the command's exit code, the total time and how much of it was preparation.

Run it again and it is much faster, because the sandbox is warm and nothing needs installing:

```text
boatparty: bp-hello-api-1 (bx_d5zd8s2j) exit 0 in 1.4s (prep 1.1s, 0% CPU steal)
```

## Three ways to give a command

`run` takes the command in one of three forms:

```bash
boatparty run -- npm test -- --watch=false    # an argument list: each word after -- is quoted
boatparty run --sh 'npm test 2>&1 | tail -n 20'   # one shell line, run as written
boatparty run test                            # a name from [commands], or a package script
```

Use `--` for ordinary commands, `--sh` when you need pipes, redirects or `&&`, and names for the commands you run often. [Running commands](/docs/running-commands) covers each form, named commands and script runners in detail.

## Exit codes

`run` exits with the remote command's own exit code. A failing test suite exits 1, a command that calls `exit 3` exits 3:

```bash
boatparty run -- node -e 'process.exit(3)'; echo "exit=$?"
```

```text
boatparty: bp-hello-api-1 (bx_d5zd8s2j) exit 3 in 1.3s (prep 1.2s, 0% CPU steal)
exit=3
```

The one exception is **125**, which always means boatparty itself failed (for example a lost connection, a missing key or a full start window), never the command. See [Exit codes and errors](/docs/exit-codes).

## Look around

```bash
boatparty status     # this project's sandboxes, their state and auto-stop time
boatparty limits     # starts left today, running sandboxes, each project's usage
boatparty log        # your recent runs
```

```text
project hello-api
* bp-hello-api-1       bx_d5zd8s2j  running   files complete auto-stop 2026-11-26T18:35:54.447Z for /Users/you/code/hello-api
```

The `*` marks the sandbox this checkout uses. `auto-stop` is when it will stop by itself if no other command arrives.

## Leave it alone between commands

You do not need to stop the sandbox. It stops by itself 5 minutes after the last command ends, keeps its disk, and the next command resumes it. Stopping it by hand only makes the next command pay for another *start*, and starts are the scarcest thing on a boat account. [Starts, limits and costs](/docs/limits) explains why.

## What to read next

- [How boatparty works](/docs/how-it-works): what happens on every command.
- [Running commands](/docs/running-commands): named commands, `shell`, `pipe`, `fetch` and cancelling.
- [Dev servers, ports and URLs](/docs/dev-servers-and-urls): run a dev server and open it in your browser.
- [Using boatparty with coding agents](/docs/coding-agents): set up the agent skill and the rules agents should follow.
