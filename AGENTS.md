# AGENTS.md

Guidance for coding agents working in projects created from `rust-cli-template`.

## Purpose

This repository is a Rust CLI first. Release plumbing and docs already exist so product work should normally stay focused on `src/`, `tests/`, and the relevant documentation.

## Architecture

- `src/main.rs`: process boundary, error rendering, exit status.
- `src/lib.rs`: reusable application entrypoint and command dispatch.
- `src/cli.rs`: `clap` parser types and help surface.
- `src/commands/`: one module per command/domain area as the CLI grows.
- `tests/`: end-to-end binary behavior.
- `Cargo.toml`: canonical crate metadata and Rust lints.
- `scripts/`: release and build tooling (`build-macos.sh`, `install.sh`, `release/`).
- `docs/`: independent Astro/ZueDocs workspace, including its own Bun lockfile and Vercel config.
- `.github/workflows/`: CI, docs and release, all on boat.dev runners (`runs-on: boat`).

## Commands

Fast edit loop:

```bash
just check-fast
```

Full Rust validation:

```bash
just check
```

Useful scoped commands:

```bash
cargo fmt --all -- --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked <test-name>
```

Validate the docs using Astro check (a local production build is unnecessary):

```bash
just docs-check
```

A macOS binary of the working tree (built and tested on a one-shot NoMac Mac):

```bash
scripts/build-macos.sh [--no-test] [--out PATH]
```

## Coding conventions

- Use Rust 2024 idioms and the pinned toolchain in `rust-toolchain.toml`.
- Keep `main.rs` tiny. Put behavior in the library and command/domain modules so it is testable without process plumbing.
- Prefer typed `clap` arguments and subcommands over manual argument parsing.
- Preserve useful command-local `--help` output and examples as the surface grows.
- `unsafe_code` is forbidden; `dbg!`, `todo!`, and `unimplemented!` are denied by project lints.
- Keep `Cargo.lock` committed: these repos ship applications, not just libraries.

## Docs boundary

Keep all web tooling in `docs/`: package metadata, Bun dependencies, Astro config, generated Astro state, and Vercel config.

The `.github/workflows/docs.yml` job is triggered by `docs/**` changes to
`main` and calls Vercel CLI for production deployments only on those pushes.
Other pushes create no Vercel deployment and use no deployment quota. Keep the
Vercel GitHub connection and Root Directory `docs`, but preserve
`git.deploymentEnabled: false` and the absence of `ignoreCommand` in
`docs/vercel.json` to avoid duplicate, quota-consuming Git deployments. Before
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

Nothing is compiled on a developer machine, for releases or otherwise. Linux
release binaries (x86_64, aarch64) are cross-built with cargo-zigbuild against
glibc 2.28 on a boat.dev sandbox; macOS binaries (arm64, x86_64) are built and
tested natively on a one-shot NoMac Mac by `scripts/build-macos.sh`. Each
machine exists only for its job.

`.github/workflows/release.yml` is maintained by hand (there is no cargo-dist
and no npm package). A tag `vX.Y.Z` matching `Cargo.toml` publishes a GitHub
Release with `boatparty-<version>-<target>.tar.gz` archives, `install.sh` and
`SHA256SUMS`; a pull request touching the release machinery runs the same
builds as a dry run. The repository is private, so users install with
`gh release download -R amxv/boatparty -p install.sh -O - | bash`
(`scripts/install.sh`, checksum-verified, into `~/.local/bin`). The macOS job
needs the `NOMAC_API_KEY` Actions secret.

For any request to cut or ship a release, read and follow `.agents/skills/release/SKILL.md`.
