---
title: Releases and installation
description: How boatparty is built, released and installed without compiling anything on a developer machine.
order: 3
category: Distribution
summary: Boat sandboxes build Linux, NoMac builds macOS, GitHub Releases host the archives.
---

## Install

The repository is private, so installation goes through the GitHub CLI and whatever access `gh auth login` already has:

```bash
gh release download -R amxv/boatparty -p install.sh -O - | bash
```

`install.sh` picks the archive for this machine (macOS or Linux, arm64 or x86_64), checks it against the release's `SHA256SUMS` and installs `boatparty` into `~/.local/bin`. Options: `--version vX.Y.Z` for a specific release, `--dir DIR` for another location. Running it again upgrades.

## Where builds run

Nothing is compiled on a developer machine.

| Output | Built on | How |
| --- | --- | --- |
| Linux x86_64 and aarch64 | a boat.dev sandbox (`runs-on: boat`) | `cargo zigbuild` against glibc 2.28, so the binaries run on older distributions. aarch64 is built, not test-run (boat is x86_64 only) |
| macOS arm64 and x86_64 | a one-shot NoMac Mac | `scripts/build-macos.sh` builds natively, runs the test suite on macOS and ad-hoc signs the binaries |
| CI checks (fmt, clippy, tests, docs) | a boat.dev sandbox per job | `.github/workflows/ci.yml` and `docs.yml`, on every branch |

Every boat sandbox and NoMac Mac exists only for its job and is deleted or stopped when the job ends.

## The release workflow

`.github/workflows/release.yml` is maintained by hand. A pushed tag `vX.Y.Z` that matches the `Cargo.toml` version:

1. builds and tests Linux on a boat sandbox and packages both Linux targets;
2. builds and tests macOS on NoMac (the `NOMAC_API_KEY` Actions secret) and packages both macOS targets;
3. creates the GitHub Release with `boatparty-<version>-<target>.tar.gz` for each target, `install.sh` and `SHA256SUMS`. The release body is the version's section of the changelog. A version with a `-` suffix becomes a prerelease.

A manual run, or a pull request that changes the release machinery, does steps 1 and 2 and stops before publishing.

## Cutting a release

Update the version in `Cargo.toml` and `Cargo.lock`, write the changelog section, push to `main`, then:

```bash
just release-tag 0.2.0
```

`release-tag` checks the version against `Cargo.toml` before pushing `v0.2.0`. Agents follow `.agents/skills/release/SKILL.md`.

## A macOS binary of a working tree

For live testing of unreleased code, `scripts/build-macos.sh` (or `just build-macos`) builds the current working tree, uncommitted files included, on a one-shot NoMac Mac and writes `target/nomac/boatparty-<target>`. `--out PATH` also installs the binary there; `--no-test` skips the macOS test run.
