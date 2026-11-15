#!/usr/bin/env bash
# Build boatparty natively for macOS on a one-shot NoMac Mac, so nothing
# compiles on the developer's machine.
#
# Builds the working tree of the current Git toplevel as it is on disk:
# uncommitted and untracked files are included, gitignored files are not
# (`nomac run` uploads the folder the way `nomac sync .` does). The Mac stops
# itself when the command ends. A named NoMac drive keeps the Rust toolchain,
# the Cargo registry and the target directory, so later runs are incremental.
#
# Usage: scripts/build-macos.sh [options]
#   --target TRIPLE    aarch64-apple-darwin (default) or x86_64-apple-darwin;
#                      repeat for both
#   --no-test          skip `cargo test --locked` (tests run on the Mac's host
#                      target by default)
#   --out PATH         also copy the first target's binary to PATH
#   --max-cost DOLLARS NoMac spend cap for this run (default 1)
#   --timeout SECONDS  command limit on the Mac (default 1800)
#   --drive NAME       NoMac drive for caches (default boatparty-build;
#                      "none" runs without a drive, fully cold)
#
# Output, in the built worktree: target/nomac/boatparty-<target> (ad-hoc
# signed) and target/nomac/SHA256SUMS. Exit status is the remote build's.
# Needs the `nomac` CLI signed in (or NOMAC_API_KEY) and, by default, Git.
set -euo pipefail

targets=()
run_tests=1
out=""
max_cost=1
timeout=1800
drive=boatparty-build

die() { echo "build-macos: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --target) [ $# -ge 2 ] || die "--target needs a value"; targets+=("$2"); shift 2 ;;
    --no-test) run_tests=0; shift ;;
    --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
    --max-cost) [ $# -ge 2 ] || die "--max-cost needs a value"; max_cost="$2"; shift 2 ;;
    --timeout) [ $# -ge 2 ] || die "--timeout needs a value"; timeout="$2"; shift 2 ;;
    --drive) [ $# -ge 2 ] || die "--drive needs a value"; drive="$2"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done
[ ${#targets[@]} -gt 0 ] || targets=(aarch64-apple-darwin)
for t in "${targets[@]}"; do
  case "$t" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) die "unsupported target: $t" ;;
  esac
done

command -v nomac >/dev/null 2>&1 || die "the nomac CLI is not installed (npm install -g @nomac/cli)"
root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "run this inside a Git worktree"
[ -f "$root/Cargo.toml" ] || die "$root has no Cargo.toml"
cd "$root"

# Runs on the Mac, in the uploaded project folder. Kept inline so this script
# can build trees that do not contain it.
# shellcheck disable=SC2016
remote='
set -euo pipefail
run_tests="$1"; shift
if [ -d "$HOME/drive" ]; then base="$HOME/drive/.boatparty-build"; else base="$HOME/.boatparty-build"; fi
mkdir -p "$base"
export RUSTUP_HOME="$base/rustup" CARGO_HOME="$base/cargo" CARGO_TARGET_DIR="$base/target"
export PATH="$CARGO_HOME/bin:$PATH" CARGO_TERM_COLOR=never
if [ ! -x "$CARGO_HOME/bin/rustup" ]; then
  echo "build-macos: installing rustup" >&2
  curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none >&2
fi
# Installs the channel pinned by rust-toolchain.toml when it is missing.
rustup toolchain install >&2
for t in "$@"; do rustup target add "$t" >&2; done
start=$(date +%s)
for t in "$@"; do
  echo "build-macos: cargo build --release --locked --target $t" >&2
  cargo build --release --locked --target "$t"
done
if [ "$run_tests" = 1 ]; then
  echo "build-macos: cargo test --locked" >&2
  cargo test --locked
fi
rm -rf target/nomac && mkdir -p target/nomac
for t in "$@"; do
  cp "$CARGO_TARGET_DIR/$t/release/boatparty" "target/nomac/boatparty-$t"
  codesign --force --sign - "target/nomac/boatparty-$t"
done
(cd target/nomac && shasum -a 256 boatparty-* > SHA256SUMS)
file target/nomac/boatparty-* >&2
echo "build-macos: remote work took $(( $(date +%s) - start ))s; drive usage:" >&2
du -sh "$base" >&2 || true
'

drive_args=()
[ "$drive" = none ] || drive_args=(--drive "$drive")

echo "build-macos: building $root for ${targets[*]} on a one-shot NoMac Mac (max \$$max_cost)" >&2
rm -rf "$root/target/nomac"
status=0
nomac run --max-cost "$max_cost" --timeout "$timeout" "${drive_args[@]}" \
  --pull target/nomac -- bash -c "$remote" build-macos "$run_tests" "${targets[@]}" || status=$?
if [ "$status" -ne 0 ]; then
  echo "build-macos: remote build failed (exit $status)" >&2
  exit "$status"
fi

( cd "$root/target/nomac" && shasum -a 256 -c SHA256SUMS >&2 ) || die "checksum mismatch after pull"
chmod +x "$root"/target/nomac/boatparty-*
if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  cp "$root/target/nomac/boatparty-${targets[0]}" "$out.tmp.$$"
  mv -f "$out.tmp.$$" "$out"
  echo "build-macos: installed ${targets[0]} binary at $out" >&2
fi
echo "build-macos: done; binaries in $root/target/nomac" >&2
