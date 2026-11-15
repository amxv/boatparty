#!/usr/bin/env bash
# Run boatparty's checks on this workspace's boat sandbox with boatparty
# itself (`boatparty cheers`), so nothing compiles on the developer's machine.
# The working tree is synced first, uncommitted files included.
#
# Usage: scripts/remote-check.sh [MODE] [ARGS...]
#   check            the full gate, the same as CI: cargo fmt --check, clippy
#                    -D warnings, cargo test, shellcheck of scripts/**/*.sh,
#                    and the docs checks (default)
#   check --no-docs  the same without the docs checks
#   fast             cargo fmt --check and clippy only
#   test [ARGS...]   cargo test --locked ARGS (for example a test name filter)
#
# Uses the boatparty binary in $BOATPARTY (default: `boatparty` on PATH, else
# ~/.local/bin/boatparty) with the caller's BOATPARTY_HOME (default: the shared login), and
# needs a boatparty.toml in the worktree.
#
# Exit status: the remote checks' (0 when all pass); 125 when boatparty
# itself failed (sandbox, connection, limits), which is not a failing check:
# retry once, then report it.
set -euo pipefail

die() { echo "remote-check: $*" >&2; exit 2; }

mode="${1:-check}"
[ $# -eq 0 ] || shift
docs=1

case "$mode" in
  -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
  check)
    for arg in "$@"; do
      case "$arg" in
        --no-docs) docs=0 ;;
        *) die "unknown check option: $arg" ;;
      esac
    done
    ;;
  fast) [ $# -eq 0 ] || die "fast takes no arguments" ;;
  test) ;;
  *) die "unknown mode: $mode (see --help)" ;;
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

quote() { printf '%q ' "$@"; }

# boat images set RUSTUP_HOME but not CARGO_HOME, which some rustup commands
# need (until the Phase 9 rust preset exports it). The remote snippets below
# are single-quoted on purpose: they expand on the sandbox.
# shellcheck disable=SC2016
prelude='set -euo pipefail
export CARGO_HOME="${CARGO_HOME:-/usr/local/cargo}" CARGO_TERM_COLOR=always
rustup toolchain install >/dev/null 2>&1 || rustup toolchain install >&2
step() { printf "\n== remote-check: %s\n" "$*" >&2; }'

fmt_clippy='step "cargo fmt --check"; cargo fmt --all -- --check
step "cargo clippy"; cargo clippy --locked --all-targets -- -D warnings'

# shellcheck disable=SC2016
shellcheck_step='step "shellcheck"
if ! command -v shellcheck >/dev/null 2>&1; then
  sudo apt-get install -y -qq --no-install-recommends shellcheck >/dev/null 2>&1 \
    || { sudo apt-get update -qq && sudo apt-get install -y -qq --no-install-recommends shellcheck >/dev/null; }
fi
shopt -s nullglob globstar
scripts=(scripts/**/*.sh docs/public/install.sh)
if [ ${#scripts[@]} -gt 0 ]; then shellcheck "${scripts[@]}"; fi'

docs_step='step "docs: bun install, test:vercel, astro check"
(cd docs && bun install --frozen-lockfile >/dev/null && bun run test:vercel && bun run check)'

case "$mode" in
  check)
    line="$prelude
$fmt_clippy
step \"cargo test\"; cargo test --locked
$shellcheck_step"
    [ "$docs" -eq 0 ] || line="$line
$docs_step"
    ;;
  fast) line="$prelude
$fmt_clippy" ;;
  test) line="$prelude
step \"cargo test $*\"; cargo test --locked $(quote "$@")" ;;
esac
line="$line
step \"all passed\""

status=0
"$bp" cheers --sh "$line" || status=$?
if [ "$status" -eq 0 ]; then
  echo "remote-check: $mode passed" >&2
elif [ "$status" -eq 125 ]; then
  echo "remote-check: boatparty itself failed (exit 125): not a check failure; retry once, then report it" >&2
else
  echo "remote-check: $mode failed (exit $status)" >&2
fi
exit "$status"
