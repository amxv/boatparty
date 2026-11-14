#!/usr/bin/env bash
# The boatparty runner: the remote half of boatparty.
#
# The client embeds this file, uploads it to the sandbox as
# /home/user/.boatparty/bin/runner-<content-hash>.sh and runs it by path, so
# the sandbox always runs the matching version and no process command line
# carries the script text.
#
# Invocation: bash runner-<hash>.sh <mode> <project> <args...>
# Modes: sync.
#
# Results travel on stderr as frames keyed by a per-invocation nonce:
#   trailer, always last: "\n<nonce> <exit-code> <resulting-tree> <patch-bytes>\n"
#   or, with exit 97:     "\n<nonce> needfull\n"
# Everything else on stderr, and all of stdout, passes through untouched.
#
# Tests run this script locally: BOATPARTY_RUNNER_REPO and
# BOATPARTY_RUNNER_STATE replace the checkout and state directories.
#
# Keep it compatible with bash 3.2 (macOS /bin/bash), which runs the tests.
set -euo pipefail

mode="${1:-}"
project="${2:-}"
if [[ -z "$mode" || -z "$project" ]]; then
  echo "boatparty runner: usage: runner.sh <mode> <project> <args...>" >&2
  exit 2
fi
shift 2

repo="${BOATPARTY_RUNNER_REPO:-/home/user/$project}"
state="${BOATPARTY_RUNNER_STATE:-/home/user/.boatparty}"
export PATH="/usr/local/cargo/bin:$HOME/.cargo/bin:$PATH"
mkdir -p "$state/out"

# The private index the checkout is staged into, so the real index is free to
# match HEAD.
sync_index() { printf '%s/.git/boatparty-index' "$repo"; }

# A build can create and delete files while the private index is refreshed;
# git then fails on a path that vanished. Retry briefly.
stage_all() {
  local idx="$1" _
  for _ in 1 2 3 4 5; do
    GIT_INDEX_FILE="$idx" git add -A 2>/dev/null && return 0
    sleep 0.2
  done
  GIT_INDEX_FILE="$idx" git add -A
}

# Commands for one workspace may overlap, so every step that touches the
# private index or moves the checkout holds this lock.
lock_checkout() {
  exec 9>"$state/checkout.lock"
  if command -v flock >/dev/null 2>&1; then flock 9; fi
}
unlock_checkout() { exec 9>&-; }

needfull() {
  printf '\n%s needfull\n' "$1" >&2
  exit 97
}

trailer() {
  printf '\n%s %s %s %s\n' "$1" "$2" "$3" "$4" >&2
}

ensure_repo() {
  if [[ ! -d "$repo/.git" ]]; then
    mkdir -p "$repo"
    git init -q "$repo"
    git -C "$repo" config core.checkStat minimal
    git -C "$repo" config core.trustctime false
    git -C "$repo" config advice.detachedHead false
  fi
}

# Mirror the client's branches, tags and default remote branch: set every
# listed ref and delete mirrored refs the client no longer has. The
# checked-out branch follows HEAD and is never deleted here.
mirror_refs() {
  local branch="$1" refs_file="$2"
  {
    awk 'NF == 2 { print "update " $2 " " $1 }' "$refs_file"
    git for-each-ref --format='%(refname)' refs/heads refs/tags refs/remotes |
      awk -v keep="refs/heads/$branch" -v list="$refs_file" '
        FILENAME == list { if (NF == 2) want[$2] = 1; next }
        !($0 in want) && $0 != keep { print "delete " $0 }
      ' "$refs_file" -
  } | git update-ref --no-deref --stdin
}

# Moves the working tree to the snapshot tree. Only paths whose content
# differs are written or deleted, so unchanged files keep their mtimes. The
# real index ends up matching HEAD, so `git status` equals the client's.
apply_snapshot() {
  local wip="$1" head="$2" branch="$3" refs_file="$4" idx
  idx="$(sync_index)"
  cd "$repo"
  if [[ -n "$refs_file" ]]; then
    mirror_refs "$branch" "$refs_file"
  fi
  git update-ref refs/boatparty/wip "$wip"
  git update-ref refs/boatparty/head "$head"
  stage_all "$idx"
  GIT_INDEX_FILE="$idx" git read-tree --reset -u "$wip"
  if [[ -n "$branch" ]]; then
    git update-ref "refs/heads/$branch" "$head"
    git symbolic-ref HEAD "refs/heads/$branch"
  else
    git update-ref --no-deref HEAD "$head"
  fi
  git reset -q
}

# Takes in the objects and refs of one sync and moves the checkout.
#
# stdin: when refs_mode is "send", lines "<sha> <ref>" for every mirrored
# ref; then one empty line; then, when with_pack is 1, a thin pack.
receive() {
  local nonce="$1" with_pack="$2" wip="$3" head="$4" branch="$5" refs_mode="$6"
  local refs_file="" line missing
  local ref_shas=()

  # bash reads a pipe one byte at a time, so the pack after the header is
  # left intact for index-pack.
  if [[ "$refs_mode" == send ]]; then
    refs_file="$state/out/$nonce.refs"
    : >"$refs_file"
  fi
  while IFS= read -r line; do
    [[ -z "$line" ]] && break
    if [[ -n "$refs_file" ]]; then
      printf '%s\n' "$line" >>"$refs_file"
      ref_shas+=("${line%% *}")
    fi
  done

  ensure_repo
  if [[ "$with_pack" == 1 ]]; then
    if ! git -C "$repo" index-pack --stdin --fix-thin >/dev/null 2>"$state/out/$nonce.pack-error"; then
      needfull "$nonce"
    fi
    rm -f "$state/out/$nonce.pack-error"
  fi
  # index-pack does not prove the new history is complete: a thin pack built
  # against a commit this repository lacks still indexes. Walk only the
  # objects not reachable from existing refs.
  if ! missing="$(git -C "$repo" rev-list --objects --missing=print "$wip" "$head" \
    ${ref_shas[@]+"${ref_shas[@]}"} --not --all 2>/dev/null)"; then
    needfull "$nonce"
  fi
  case $'\n'"$missing" in
    *$'\n?'*) needfull "$nonce" ;;
  esac

  lock_checkout
  apply_snapshot "$wip" "$head" "$branch" "$refs_file"
  unlock_checkout
  [[ -n "$refs_file" ]] && rm -f "$refs_file"
  return 0
}

# sync <nonce> <with-pack 0|1> <wip> <head> <branch or ""> <refs: send|keep>
mode_sync() {
  local nonce="$1" wip="$3"
  receive "$@"
  trailer "$nonce" 0 "$(git -C "$repo" rev-parse "$wip^{tree}")" 0
}

case "$mode" in
  sync) mode_sync "$@" ;;
  *)
    echo "boatparty runner: unknown mode $mode" >&2
    exit 2
    ;;
esac
