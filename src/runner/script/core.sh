#!/usr/bin/env bash
# The boatparty runner: the remote half of boatparty.
#
# The client embeds this file, uploads it to the sandbox as
# /home/user/.boatparty/bin/runner-<content-hash>.sh and runs it by path, so
# the sandbox always runs the matching version and no process command line
# carries the script text.
#
# Invocation: bash runner-<hash>.sh <mode> <project> <args...>
# Modes: sync, run, serve (a long-running `run`; see "Servers"), seed,
# fetch-patch, kill, maintain, settle (a builder's last step before its base
# is saved), credential (Git's credential helper inside a command; see
# "Build credentials").
#
# The repository keeps this script as parts (src/runner/script/*.sh) that
# the binary joins in a fixed order; only the joined text is uploaded.
#
# sync and run name a RunnerSpec: the project's resolved settings, written by
# the client to /home/user/.boatparty/specs/<hash>.sh (also named by content
# hash) and sourced here. A spec file that is missing is reported like a
# missing runner (exit 96), so the client uploads both and retries. "-"
# names no spec: nothing is applied. The spec only holds data (shell arrays);
# every workaround below is either driven by it or stack-neutral.
#
# Results travel on stderr as frames keyed by a per-invocation nonce:
#   event:                "\n<nonce>!<event>\n" (run emits "started"; serve
#                         also "ready")
#   trailer, always last: "\n<nonce> <exit-code> <resulting-tree> <patch-bytes>\n"
#   or, with exit 97:     "\n<nonce> needfull\n"
# Everything else on stderr, and all of stdout, passes through untouched.
#
# Tests run this script locally: BOATPARTY_RUNNER_REPO and
# BOATPARTY_RUNNER_STATE replace the checkout and state directories;
# BOATPARTY_RUNNER_SHM (the RAM directory), BOATPARTY_RUNNER_BOOT_ID and
# BOATPARTY_RUNNER_HYDRATING (1 or 0) stand in for the machine's.
#
# History: in "head" mode the client sends HEAD's commit and complete tree
# but none of its parents, and the checkout records HEAD as a shallow
# boundary (.git/shallow), so `git log` shows one commit and integrity checks
# find nothing missing. Old boundaries stay listed, because reflogs still
# name those commits. "full" mode drops every boundary whose parents have
# arrived, which deepens a checkout that was synced in "head" mode before.
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
mkdir -p "$state/out" "$state/in"

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

# Index a pack from stdin, or from an uploaded file (@<name> in $state/in).
take_pack() {
  local nonce="$1" pack="$2" file
  case "$pack" in
    0) return 0 ;;
    1) git -C "$repo" index-pack --stdin --fix-thin >/dev/null 2>"$state/out/$nonce.pack-error" ||
      needfull "$nonce" ;;
    @*)
      file="${pack#@}"
      case "$file" in
        "" | */* | .*) echo "boatparty runner: bad pack name $file" >&2; exit 2 ;;
      esac
      file="$state/in/$file"
      if [[ ! -f "$file" ]]; then
        echo "boatparty runner: the uploaded pack is missing" >"$state/out/$nonce.pack-error"
        needfull "$nonce"
      fi
      if ! git -C "$repo" index-pack --stdin --fix-thin <"$file" >/dev/null 2>"$state/out/$nonce.pack-error"; then
        rm -f "$file"
        needfull "$nonce"
      fi
      rm -f "$file"
      ;;
    *) echo "boatparty runner: bad pack argument $pack" >&2; exit 2 ;;
  esac
  rm -f "$state/out/$nonce.pack-error"
}

# head mode: record the HEAD commit as a shallow boundary (once it exists).
mark_shallow() {
  local head="$1" file="$repo/.git/shallow"
  git -C "$repo" cat-file -e "$head^{commit}" 2>/dev/null || return 0
  if [[ ! -f "$file" ]] || ! grep -qx "$head" "$file"; then
    printf '%s\n' "$head" >>"$file"
  fi
}

# full mode: forget every boundary whose parents are all present now.
deepen() {
  local file="$repo/.git/shallow" sha parent keep
  [[ -f "$file" ]] || return 0
  : >"$file.boatparty"
  while IFS= read -r sha; do
    [[ -z "$sha" ]] && continue
    keep=0
    if git -C "$repo" cat-file -e "$sha^{commit}" 2>/dev/null; then
      while IFS= read -r parent; do
        git -C "$repo" cat-file -e "$parent" 2>/dev/null || keep=1
      done < <(git -C "$repo" cat-file commit "$sha" | sed -n '/^$/q; s/^parent //p')
    fi
    if [[ "$keep" == 1 ]]; then printf '%s\n' "$sha" >>"$file.boatparty"; fi
  done <"$file"
  if [[ -s "$file.boatparty" ]]; then
    mv "$file.boatparty" "$file"
  else
    rm -f "$file" "$file.boatparty"
  fi
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
