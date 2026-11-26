---
title: Releases and builds
description: How boatparty is built, tested and released on GitHub-hosted runners for Linux, macOS and Windows, without compiling anything on a developer machine.
order: 90
category: Project
summary: GitHub-hosted runners build and test every target; GitHub Releases host the archives.
---

boatparty is released as prebuilt binaries on [GitHub Releases](https://github.com/amxv/boatparty/releases). To install or upgrade, see [Installation](/docs/installation). This page is for the curious and for contributors: where each binary is built, what a release contains, and how one is cut.

## What a release contains

Each release `vX.Y.Z` publishes:

| File | For |
| --- | --- |
| `boatparty-X.Y.Z-aarch64-apple-darwin.tar.gz`, `boatparty-X.Y.Z-x86_64-apple-darwin.tar.gz` | macOS on Apple silicon and Intel |
| `boatparty-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz`, `boatparty-X.Y.Z-aarch64-unknown-linux-gnu.tar.gz` | Linux with glibc 2.28 or newer |
| `boatparty-X.Y.Z-x86_64-pc-windows-msvc.zip`, `boatparty-X.Y.Z-aarch64-pc-windows-msvc.zip` | Windows x64 and Arm64, containing `boatparty.exe` |
| `install.sh`, `install.ps1` | The installers, also served by this site |
| `SHA256SUMS` | Checksums the installers verify before installing anything |

## Where builds run

Nothing is compiled on a developer machine.

| Output | Built on | How |
| --- | --- | --- |
| Linux x86_64 and aarch64 releases | GitHub's `ubuntu-latest` | the test suite, then `cargo zigbuild` against glibc 2.28, so the binaries run on older distributions. aarch64 is built, not test-run |
| macOS arm64 and x86_64 releases | GitHub's `macos-latest` | the test suite on macOS, then a native `cargo build` per target; both binaries are signed ad hoc |
| Windows x64 and Arm64 releases | GitHub’s `windows-latest` | x64 native tests including SSH to Linux, followed by MSVC builds for both architectures |
| CI checks (fmt, clippy, tests, docs) | GitHub's `ubuntu-latest` | `.github/workflows/ci.yml` and `docs.yml`, on every branch |
| Development checks and macOS binaries of a working tree | this workspace's boat sandbox, through boatparty | `just remote-check`, `just boat-build-macos` |

## The release workflow

`.github/workflows/release.yml` is maintained by hand. A pushed tag `vX.Y.Z` that matches the `Cargo.toml` version:

1. tests and builds Linux on `ubuntu-latest` and packages both Linux targets;
2. tests and builds macOS on `macos-latest` and packages both macOS targets;
3. tests and builds Windows on `windows-latest` and packages both Windows targets as ZIP files;
4. publishes the Unix tarballs, Windows ZIPs, `install.sh`, `install.ps1` and `SHA256SUMS`. The release body is the version's section of the changelog. A version with a `-` suffix becomes a prerelease.

A manual run, or a pull request that changes the release machinery, does steps 1 through 3 and stops before publishing.

## Cutting a release

Update the version in `Cargo.toml` and `Cargo.lock`, write the changelog section, push to `main`, then:

```bash
just release-tag 0.2.0
```

`release-tag` checks the version against `Cargo.toml` before pushing `v0.2.0`. Agents follow `.agents/skills/release/SKILL.md`.

## A macOS binary of a working tree

For live testing of unreleased code, `just boat-build-macos --out PATH` cross-compiles the current working tree, uncommitted files included, for `aarch64-apple-darwin` on this workspace's boat sandbox with cargo-zigbuild, fetches the binary and signs it ad hoc locally.
