---
title: Git identity and GitHub access
description: How commits made on a sandbox carry your name, how commands and syncs reach private GitHub repositories with a short-lived token that is never stored, and how to limit what that token can do.
order: 26
category: Guides
summary: git_identity, run and sync GitHub tokens, and choosing a narrower token.
---

Commands on a sandbox sometimes need Git to behave as if it were you: committing with your name, fetching a private dependency, or cloning a private submodule. boatparty provides both your Git identity and, when you allow it, a GitHub token, without ever storing the token on the sandbox.

## Your Git identity

With `[run] git_identity = true` (the default), every `run`, `serve`, `pipe` and `shell` command gets your local repository's `user.name` and `user.email`, exported as `GIT_AUTHOR_*` and `GIT_COMMITTER_*` and as Git configuration. A commit made on the sandbox (by a release script or a tool that commits) is attributed to you, exactly as locally.

Turn it off if commits on the sandbox should use another identity:

```toml
[run]
git_identity = false
```

A `[run.env]` entry can still override individual values.

## Two GitHub token settings

There are two places a GitHub token can be used, controlled separately:

| Setting | Default | Gives a token to | Used for |
| --- | --- | --- | --- |
| `[run] github_token` | `false` | Git inside your commands and steps, for the command's lifetime | Private dependencies: Cargo git dependencies, npm GitHub packages, Go private modules, submodules |
| `[sync] github_token` | `true` | The one `git fetch` that [seeds](/docs/sync-and-copy-back#seeding-from-github) a sandbox from GitHub | Seeding a private repository |

Leave both off for projects that only use public repositories.

### Private dependencies in commands

```toml
[run]
github_token = true
```

With this on, Git inside commands and steps can reach `github.com` over HTTPS as you. boatparty runs a credential helper that serves the token from memory for as long as the command runs; it is never written to the sandbox's disk, a Git config file or a remote URL. SSH-style GitHub URLs (`git@github.com:org/repo.git`) are rewritten to HTTPS so they use the token too.

Some tools need a hint to use Git for private repositories:

- **Go**: add the module path to `GOPRIVATE` so Go does not go through the public proxy:

  ```toml
  [run.env]
  GOPRIVATE = "github.com/your-org/*"
  ```

- **Cargo**: the `rust` preset already sets `CARGO_NET_GIT_FETCH_WITH_CLI = "true"`, so Cargo fetches Git dependencies with the Git CLI and its credential helper.

## Where the token comes from

When a token is about to be used, boatparty looks for one in this order:

1. `BOATPARTY_GITHUB_TOKEN`, if set;
2. the first line printed by `BOATPARTY_GITHUB_TOKEN_COMMAND`, a shell command, if set;
3. `gh auth token`, your GitHub CLI login.

These are read only when a token is actually needed, and the token stays in boatparty's memory. It never appears in an argument list, a file, `state.json`, `runs.jsonl` or a log, on your computer or on the sandbox.

## Limiting what the token can do

By default the token is your `gh` login: a long-lived OAuth token with every scope `gh` was granted, usually `repo`, which means **read and write access to every repository you can reach**. While a command runs with `[run] github_token = true`, anything that command runs on the sandbox, including dependency build scripts, could ask the credential helper for it. A seed hands it only to the one `git fetch`.

GitHub offers no way to derive a shorter-lived or narrower token from a `gh` login. To limit what a leaked token could do, give boatparty a different one:

- **A fine-grained personal access token** with read-only **Contents** access to just the repositories your builds fetch, and a short expiry:

  ```bash
  export BOATPARTY_GITHUB_TOKEN="$(op read 'op://Dev/github-boatparty/token')"
  ```

- **A command that mints a short-lived token**, for example a GitHub App installation token (valid one hour, limited to the app's repositories and permissions) from a helper script or your secret manager:

  ```bash
  export BOATPARTY_GITHUB_TOKEN_COMMAND='my-github-app-token --repo your-org/app'
  ```

On Windows, `BOATPARTY_GITHUB_TOKEN_COMMAND` runs with `sh` from your `PATH` (for example the one that comes with Git for Windows).

## Alternative: a boat environment with GitHub access

A boat [environment](/docs/secrets-and-environments) can include GitHub access of its own. A project whose `[sandbox] environment` has it can use that instead of `[run] github_token`. The difference is that boat then stores the login on the sandbox, while boatparty's token is served from memory and only while a command runs.
