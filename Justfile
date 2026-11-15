set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set positional-arguments

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

check-fast:
    cargo fmt --all -- --check
    cargo check --locked
    cargo clippy --locked --all-targets -- -D warnings

check:
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked

build:
    cargo build --locked

build-release:
    cargo build --locked --release

run *args:
    cargo run -- "$@"

docs-install:
    cd docs && bun install --frozen-lockfile

docs-dev:
    cd docs && bun run dev

docs-check:
    cd docs && bun run check

docs-build:
    cd docs && bun run build

docs-test:
    cd docs && bun run test:vercel

# Full gate (fmt, clippy, test, shellcheck, docs) on this workspace's boat sandbox via boatparty cheers.
remote-check *args:
    scripts/remote-check.sh check "$@"

# fmt --check and clippy on this workspace's boat sandbox.
remote-fast:
    scripts/remote-check.sh fast

# cargo test --locked [ARGS] on this workspace's boat sandbox.
remote-test *args:
    scripts/remote-check.sh test "$@"

# Cross-compile this tree's macOS binary on this workspace's boat sandbox (zigbuild) and fetch it.
boat-build-macos *args:
    scripts/boat-build-macos.sh "$@"

# Install the latest (or --version vX.Y.Z) release into ~/.local/bin.
install *args:
    scripts/install.sh "$@"

# Build and package every release target without publishing (needs release.yml on main).
release-dry-run:
    gh workflow run release.yml --ref "$(git rev-parse --abbrev-ref HEAD)"

release-tag version:
    #!/usr/bin/env bash
    set -euo pipefail
    actual="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
    if [[ "$actual" != "{{ version }}" ]]; then
      echo "Cargo.toml version is $actual, not {{ version }}" >&2
      exit 1
    fi
    git tag "v{{ version }}"
    git push origin "v{{ version }}"
