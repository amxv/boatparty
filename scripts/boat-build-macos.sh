#!/usr/bin/env bash
# Build a macOS binary of boatparty on this workspace's boat sandbox, with
# boatparty itself, so neither the developer's Mac nor NoMac compiles it.
#
# Builds the working tree of the current Git toplevel as it is on disk
# (uncommitted and untracked files included, ignored files not) by running
# `boatparty cheers` in the workspace's member: cargo-zigbuild cross-compiles
# for macOS, in its own target directory (target/darwin) so the member's Linux
# build stays warm. The binary comes back with `boatparty fetch`. zig and
# cargo-zigbuild are installed once, pinned and checksum-verified, under
# ~/.boatparty-tools on the member, which survives stops and resumes.
#
# Usage: scripts/boat-build-macos.sh [options]
#   --target TRIPLE   aarch64-apple-darwin (default) or x86_64-apple-darwin
#   --out PATH        where to put the binary (default:
#                     target/boat-darwin/boatparty-<target> in this worktree)
#
# Uses the boatparty binary in $BOATPARTY (default: `boatparty` on PATH, else
# ~/.local/bin/boatparty) with the caller's BOATPARTY_HOME (default: the shared login), and
# needs a boatparty.toml in the worktree. The binary is not run or tested on
# macOS here; use scripts/build-macos.sh (NoMac) for macOS tests.
#
# Exit status: the remote build's; 125 when boatparty itself failed.
set -euo pipefail

target=aarch64-apple-darwin
out=""

die() { echo "boat-build-macos: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
    --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done
case "$target" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) die "unsupported target: $target" ;;
esac

if [ -n "${BOATPARTY:-}" ]; then
  bp="$BOATPARTY"
elif command -v boatparty >/dev/null 2>&1; then
  bp="$(command -v boatparty)"
else
  bp="$HOME/.local/bin/boatparty"
fi
[ -x "$bp" ] || die "no boatparty binary at $bp (set BOATPARTY)"

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "run this inside a Git worktree"
[ -f "$root/boatparty.toml" ] || die "$root has no boatparty.toml (run boatparty init)"
cd "$root"
[ -n "$out" ] || out="$root/target/boat-darwin/boatparty-$target"

# The pinned installer travels inline, so trees without it can be built too.
installer="$(dirname "$0")/release/install-zigbuild.sh"
[ -f "$installer" ] || die "missing $installer"
installer_b64="$(base64 < "$installer" | tr -d '\n')"

remote="set -euo pipefail
export CARGO_HOME=\"\${CARGO_HOME:-/usr/local/cargo}\" CARGO_TARGET_DIR=target/darwin CARGO_TERM_COLOR=never
tools=\"\$(echo '$installer_b64' | base64 -d | bash -s -- \"\$HOME/.boatparty-tools\")\"
export PATH=\"\$tools:\$PATH\"
rustup target add $target >&2
cargo zigbuild --release --locked --target $target >&2
file target/darwin/$target/release/boatparty >&2"

echo "boat-build-macos: building $root for $target on this workspace's boat sandbox" >&2
status=0
"$bp" cheers --sh "$remote" || status=$?
[ "$status" -eq 0 ] || { echo "boat-build-macos: remote build failed (exit $status)" >&2; exit "$status"; }

mkdir -p "$(dirname "$out")"
tmp="$out.tmp.$$"
status=0
"$bp" fetch "target/darwin/$target/release/boatparty" "$tmp" || status=$?
[ "$status" -eq 0 ] || { rm -f "$tmp"; echo "boat-build-macos: fetch failed (exit $status)" >&2; exit "$status"; }
chmod 755 "$tmp"
# zig's linker signs arm64 binaries ad hoc; re-sign so the result is always
# valid for Gatekeeper's local-execution check (signing compiles nothing).
if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$tmp" 2>/dev/null || true
fi
mv -f "$tmp" "$out"
echo "boat-build-macos: $out" >&2
if [ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] && [ "$target" = aarch64-apple-darwin ]; then
  "$out" --version >&2
fi
