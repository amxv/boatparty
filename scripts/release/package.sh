#!/usr/bin/env bash
# Package one release binary as boatparty-<version>-<target>.tar.gz with the
# README and LICENSE, the layout scripts/install.sh expects.
#
# Usage: scripts/release/package.sh VERSION TARGET BINARY OUTDIR
set -euo pipefail

[ $# -eq 4 ] || { echo "usage: $0 VERSION TARGET BINARY OUTDIR" >&2; exit 2; }
version="$1" target="$2" binary="$3" outdir="$4"
root="$(cd "$(dirname "$0")/../.." && pwd)"
[ -x "$binary" ] || { echo "package: $binary is not an executable file" >&2; exit 1; }

name="boatparty-${version}-${target}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$outdir"
install -m 755 "$binary" "$stage/$name/boatparty"
cp "$root/README.md" "$root/LICENSE" "$stage/$name/"
# Deterministic ordering and ownership; mtimes stay as built.
tar -C "$stage" -czf "$outdir/$name.tar.gz" --owner=0 --group=0 "$name" 2>/dev/null \
  || tar -C "$stage" -czf "$outdir/$name.tar.gz" "$name"
echo "$outdir/$name.tar.gz"
