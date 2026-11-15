# AGENTS.md

Guidance for coding agents working on boatparty.

## Purpose

boatparty is a Rust CLI that gives any Git project a pool of warm boat.dev sandboxes: it mirrors the working tree, runs commands there with live output and real exit codes, applies file changes back, and keeps every pool and limit of one boat account in one place. The README describes the product; `docs/json.md` is the `--json` contract; `docs/boat-facts.md` records measured boat behavior the design depends on.

## Architecture

One crate, split by responsibility:

- `src/main.rs`: process boundary only. `src/lib.rs`: parses, dispatches, turns errors into exit status 125 and the JSON error shape.
- `src/cli.rs`: every `clap` type and help text. `src/commands/`: one module per command (`base/` for `base build|status|rm`); `session.rs` is the `session` lease shared by `board`, `fetch`, `pipe`, `expose` and `forward`.
- `src/provider/`: the only code that knows boat. The `Provider` trait, typed sandbox states and errors (`error.rs`), and `boat/` (HTTP client, error mapping, sandboxes, snapshots, signed file URLs, hosting, the key-scope probes). Nothing outside it names a boat URL, state or error code.
- `src/account/`: `account.toml` (`config.rs`, comment-preserving edits in `edit.rs`), the API key (`credentials.rs`), resolved per-project limits (`limits.rs`).
- `src/state/`: `state.json` and its advisory lock. The lock is held only for one read-modify-write, never across a network call or a child process.
- `src/pool/`: acquisition (`policy.rs` is the pure decision, `acquire.rs` applies it), bring-up (`bringup.rs`), leases, heartbeats and the detached idle helper (`lease.rs`), reconciliation with the provider's labels (`reconcile.rs`), deletion (`remove.rs`).
- `src/starts/`: the start ledger. `admission.rs` is the pure admit, wait or refuse computation.
- `src/transport/`: OpenSSH configs, pinned host keys, the shared control master (`ssh.rs`), port forwards (`forward.rs`).
- `src/sync/`: the Git-native mirror. Snapshot into a private index, thin packs, the runner exchange and its framing (`exchange.rs`), sync and run plumbing (`mirror.rs`), piped input (`pipe.rs`), GitHub seeding (`seed.rs`), build credentials (`credentials.rs`), copy-back (`pullback.rs`).
- `src/runner/`: the remote half. `script/*.sh` are parts of one bash script joined at build time, uploaded under its content hash and run by path; `spec.rs` turns the resolved project configuration into a data-only `RunnerSpec`; `trailer.rs` filters the nonce-keyed event and trailer frames out of stderr.
- `src/config/`: the strict `boatparty.toml` schema, layering of built-in, user and project presets (`presets/*.toml` are embedded), stack detection for `init`.
- `src/base/`: base snapshot records and staleness. `src/project.rs`: workspace and project file. `src/runs.rs`: `runs.jsonl`. `src/stream.rs`: streaming output and JSON frames. `src/output.rs`: text and JSON output conventions. `src/paths.rs`: every local path.
- `skill/SKILL.md`: the agent skill, embedded in the binary and written by `boatparty skill`.
- `tests/`: end-to-end tests. `tests/common/` has a fake boat API, a fake `ssh` that runs remote commands locally against a scratch "sandbox", and a local runner host. `tests/json.rs` checks every command's `--json` output against `docs/json.md`.
- `docs/`: independent Astro/ZueDocs workspace (see Docs boundary). `scripts/` and `.github/` belong to the release machinery (see Release boundary).

## Commands

Nothing compiles on the developer machine: no `cargo build`, `check`, `clippy` or `test`, no `bun` or `astro`. Only `cargo fmt` runs locally. Checks run on this workspace's boat sandbox through boatparty (`scripts/remote-check.sh` runs them with `boatparty cheers`):

```bash
cargo fmt --all                 # local, no compilation
just remote-fast                # fmt --check and clippy -D warnings on boat
just remote-test [FILTER...]    # cargo test --locked on boat, for example `just remote-test --test pipe`
just remote-check               # the full gate, the same as CI: fmt, clippy, tests, shellcheck, docs check
just boat-build-macos --out PATH   # a macOS binary of this working tree, built on boat
```

The checks use the boatparty binary in `$BOATPARTY`, else `boatparty` on `PATH`. Use a binary built from a landed commit, never one built from the tree being edited. Exit 125 from a check means boatparty or the sandbox failed, not a check: retry once, then report it (and fix it if it is a boatparty bug). Live tests of new code use a binary from `just boat-build-macos` under a scratch `BOATPARTY_HOME`, against scratch repositories only.

## Coding conventions

- Rust 2024 with the pinned toolchain in `rust-toolchain.toml`. Synchronous code with threads; no async runtime.
- `unsafe_code` is forbidden; `dbg!`, `todo!` and `unimplemented!` are denied. Errors are a typed `AppError` with a stable `code`; every boatparty failure exits 125.
- Every command accepts `--json`. A change to a JSON shape changes `docs/json.md` in the same commit; `tests/json.rs` fails otherwise. Error codes are listed there too.
- The tool's own messages go to stderr prefixed `boatparty: `; stdout belongs to the result or the remote command.
- Secrets (the API key, GitHub tokens, signed URLs, `expose` URLs, `pipe` input) never reach argv, logs, `state.json` or `runs.jsonl`.
- The warm path of `cheers`, `board` and `pipe` makes no provider call before the command starts. Provider TTL changes happen in the background or in the detached idle helper.
- The runner stays stack-neutral and bash 3.2 compatible: anything specific to a language belongs in a preset, and the runner reads it from the spec. Keep its contract (modes, frames, exit 96 and 97) in sync with the comments at the top of `src/runner/script/core.sh`.
- Pure decisions (acquisition, admission, staleness) stay pure functions with table tests; side effects sit around them.
- New code files stay at or below 1,000 lines. `Cargo.lock` stays committed.

## Docs boundary

Keep all web tooling in `docs/`: package metadata, Bun dependencies, Astro config, generated Astro state, and Vercel config.

The `.github/workflows/docs.yml` job, on a GitHub-hosted runner, is triggered
by `docs/**` changes: pushes to `main` deploy production, and same-repository
pull requests get a Preview deployment whose URL is commented on the PR. A
later push never cancels a production deploy in flight. Other pushes create no
Vercel deployment and use no deployment quota. The Vercel project's Git
integration is disconnected (keep it that way, with Root Directory `docs`), and
`docs/vercel.json` keeps `git.deploymentEnabled: false` and no `ignoreCommand`:
an ignored build still creates a counted deployment. Before
the first deployment, store a project-scoped `VERCEL_TOKEN` as a GitHub
Actions **secret** and the Vercel IDs (`VERCEL_ORG_ID`, `VERCEL_PROJECT_ID`) as
Actions **variables**; until `VERCEL_PROJECT_ID` exists the deploy steps are
skipped and only the check runs. Never print or commit credentials. GitHub Actions builds
the static Astro output and deploys it with `--prebuilt`, compatible with
restricted project tokens; local agents only run `just docs-check`. The build
runs within `docs/`, but deployment must run from repository root with the
bundle copied from `docs/.vercel/output` to `.vercel/output`, because Vercel
already has Root Directory `docs` configured. Never deploy from `docs/` with
that configuration; it resolves the project to `docs/docs`.
Run `just docs-test` to verify the GitHub filter and the retained local diff
helper (`docs/scripts/should-build.mjs`). Never re-enable automatic Vercel Git
deployments without first disabling the Actions deployment path.

## Release boundary

Nothing is compiled on a developer machine, for releases or otherwise.
Development compiles, checks and tests run on boat through boatparty;
releases run only on GitHub-hosted runners. Linux release binaries (x86_64,
aarch64) are tested and cross-built with cargo-zigbuild against glibc 2.28 on
`ubuntu-latest`; macOS binaries (arm64, x86_64) are tested and built natively
on `macos-latest`, and signed ad hoc.

`.github/workflows/release.yml` is maintained by hand (there is no cargo-dist
and no npm package). A tag `vX.Y.Z` matching `Cargo.toml` publishes a GitHub
Release with `boatparty-<version>-<target>.tar.gz` archives, `install.sh` and
`SHA256SUMS`; a pull request touching the release machinery runs the same
builds as a dry run. Users install with
`curl -fsSL https://boatparty.ashray.xyz/install.sh | sh`: `docs/public/install.sh`,
served by the docs site and attached to every release, checksum-verified,
into `~/.local/bin`; it downloads through a signed-in `gh` while the
repository is private, else with `curl`. The release
needs no secret besides the workflow's own `GITHUB_TOKEN`.

For any request to cut or ship a release, read and follow `.agents/skills/release/SKILL.md`.
