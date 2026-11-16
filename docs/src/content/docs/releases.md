---
title: Releases and installation
description: How boatparty is built, released and installed without compiling anything on a developer machine.
order: 3
category: Distribution
summary: GitHub-hosted runners build and test Linux and macOS, GitHub Releases host the archives.
---

## Install

```bash
curl -fsSL https://boatparty.ashray.xyz/install.sh | sh
```

The script (`docs/public/install.sh`, served by this site and attached to every release as `install.sh`) picks the archive for this machine (macOS or Linux, arm64 or x86_64), checks it against the release's `SHA256SUMS` and installs `boatparty` into `~/.local/bin`, replacing the old binary in one rename. Options, after `sh -s --`: `--version vX.Y.Z` for a specific release, `--dir DIR` for another location (or `BOATPARTY_VERSION` and `BOATPARTY_INSTALL_DIR`). Running it again upgrades. It needs only `curl`, `tar` and a shell.

## Where builds run

Nothing is compiled on a developer machine.

| Output | Built on | How |
| --- | --- | --- |
| Linux x86_64 and aarch64 releases | GitHub's `ubuntu-latest` | the test suite, then `cargo zigbuild` against glibc 2.28, so the binaries run on older distributions. aarch64 is built, not test-run |
| macOS arm64 and x86_64 releases | GitHub's `macos-latest` | the test suite on macOS, then a native `cargo build` per target; both binaries are signed ad hoc |
| CI checks (fmt, clippy, tests, docs) | GitHub's `ubuntu-latest` | `.github/workflows/ci.yml` and `docs.yml`, on every branch |
| Development checks and macOS binaries of a working tree | this workspace's boat sandbox, through boatparty | `just remote-check`, `just boat-build-macos` |

## The release workflow

`.github/workflows/release.yml` is maintained by hand. A pushed tag `vX.Y.Z` that matches the `Cargo.toml` version:

1. tests and builds Linux on `ubuntu-latest` and packages both Linux targets;
2. tests and builds macOS on `macos-latest` and packages both macOS targets;
3. creates the GitHub Release with `boatparty-<version>-<target>.tar.gz` for each target, `install.sh` and `SHA256SUMS`. The release body is the version's section of the changelog. A version with a `-` suffix becomes a prerelease.

A manual run, or a pull request that changes the release machinery, does steps 1 and 2 and stops before publishing.

## Cutting a release

Update the version in `Cargo.toml` and `Cargo.lock`, write the changelog section, push to `main`, then:

```bash
just release-tag 0.2.0
```

`release-tag` checks the version against `Cargo.toml` before pushing `v0.2.0`. Agents follow `.agents/skills/release/SKILL.md`.

## A macOS binary of a working tree

For live testing of unreleased code, `just boat-build-macos --out PATH` cross-compiles the current working tree, uncommitted files included, for `aarch64-apple-darwin` on this workspace's boat sandbox with cargo-zigbuild, fetches the binary and signs it ad hoc locally.
