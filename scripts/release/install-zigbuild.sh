#!/usr/bin/env bash
# Install pinned, checksum-verified zig and cargo-zigbuild on an x86_64 Linux
# host (a boat sandbox or CI runner) for cross-compiling release binaries.
#
# Usage: scripts/release/install-zigbuild.sh [PREFIX]
#   PREFIX defaults to ~/.boatparty-tools. Prints the bin directory to add to
#   PATH on stdout; with GITHUB_PATH set it is also added for later steps.
set -euo pipefail

zig_version=0.15.2
zig_sha256=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
zigbuild_version=0.24.1
zigbuild_sha256=f09b180a517af0e4dd4bffc921bb88e5ab705452a9dd948c61e489b3020c68c2

prefix="${1:-$HOME/.boatparty-tools}"
[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] || { echo "install-zigbuild: x86_64 Linux only" >&2; exit 1; }

fetch() { # url sha256 dest
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum -c - >&2
}

mkdir -p "$prefix/bin"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ ! -x "$prefix/zig-$zig_version/zig" ]; then
  fetch "https://ziglang.org/download/$zig_version/zig-x86_64-linux-$zig_version.tar.xz" \
    "$zig_sha256" "$work/zig.tar.xz"
  tar -xJf "$work/zig.tar.xz" -C "$work"
  rm -rf "$prefix/zig-$zig_version"
  mv "$work/zig-x86_64-linux-$zig_version" "$prefix/zig-$zig_version"
fi
ln -sf "$prefix/zig-$zig_version/zig" "$prefix/bin/zig"

if [ "$("$prefix/bin/cargo-zigbuild" --version 2>/dev/null | awk '{ print $2 }')" != "$zigbuild_version" ]; then
  fetch "https://github.com/rust-cross/cargo-zigbuild/releases/download/v$zigbuild_version/cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz" \
    "$zigbuild_sha256" "$work/zigbuild.tar.xz"
  tar -xJf "$work/zigbuild.tar.xz" -C "$work"
  install -m 755 "$work/cargo-zigbuild-x86_64-unknown-linux-musl/cargo-zigbuild" "$prefix/bin/cargo-zigbuild"
fi

[ -z "${GITHUB_PATH:-}" ] || echo "$prefix/bin" >> "$GITHUB_PATH"
echo "$prefix/bin"
