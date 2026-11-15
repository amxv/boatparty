#!/usr/bin/env bash
# The boatparty runner: the remote half of boatparty.
#
# The client embeds this file, uploads it to the sandbox as
# /home/user/.boatparty/bin/runner-<content-hash>.sh and runs it by path, so
# the sandbox always runs the matching version and no process command line
# carries the script text.
#
# Invocation: bash runner-<hash>.sh <mode> <project> <args...>
# Modes: sync, run, fetch-patch, kill.
#
# Results travel on stderr as frames keyed by a per-invocation nonce:
#   event:                "\n<nonce>!<event>\n" (run emits "started")
#   trailer, always last: "\n<nonce> <exit-code> <resulting-tree> <patch-bytes>\n"
#   or, with exit 97:     "\n<nonce> needfull\n"
# Everything else on stderr, and all of stdout, passes through untouched.
#
# Tests run this script locally: BOATPARTY_RUNNER_REPO and
# BOATPARTY_RUNNER_STATE replace the checkout and state directories.
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
export PATH="/usr/local/cargo/bin:$HOME/.cargo/bin:$PATH"
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
# ref; then one empty line; then, when pack is 1, a thin pack. A pack of
# "@<name>" was uploaded to $state/in/<name> instead.
receive() {
  local nonce="$1" pack="$2" wip="$3" head="$4" branch="$5" refs_mode="$6" history="$7"
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
  find "$state/in" -maxdepth 1 -type f -mmin +1440 -delete 2>/dev/null || true
  take_pack "$nonce" "$pack"
  case "$history" in
    head) mark_shallow "$head" ;;
    full) deepen ;;
    *) echo "boatparty runner: unknown history mode $history" >&2; exit 2 ;;
  esac
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

# sync <nonce> <pack: 0|1|@name> <wip> <head> <branch or ""> <refs: send|keep> <history: head|full>
mode_sync() {
  local nonce="$1" wip="$3"
  receive "$@"
  trailer "$nonce" 0 "$(git -C "$repo" rev-parse "$wip^{tree}")" 0
}

# Signal the command's whole process group: TERM, then KILL for whatever is
# still there a few seconds later.
kill_group() {
  local pid="$1" _
  # Right after the start the command may not have made its own session yet;
  # then the leader itself is signalled, before it can start anything.
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || return 0
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 -- "-$pid" 2>/dev/null || return 0
    sleep 0.5
  done
  kill -KILL -- "-$pid" 2>/dev/null || true
}

# Writes the command's changes to the checkout as a patch against the latest
# sync, under the checkout lock, then prints the trailer. A cancelled command
# sends nothing back. The trailer is always printed, even when the
# bookkeeping fails, so the command's exit code is never lost.
finish() {
  local nonce="$1" rc="$2" wip="$3" idx base after="-" bytes=0 patch
  patch="$state/out/$nonce.patch"
  lock_checkout
  idx="$(sync_index)"
  cd "$repo"
  # Compare with the latest sync, not this command's: if another command
  # re-synced the checkout meanwhile, those edits are not this command's
  # changes and must not come back as a patch.
  if base="$(git rev-parse -q --verify refs/boatparty/wip)"; then
    if [[ "$base" != "$wip" ]]; then
      echo "boatparty: note: the checkout was re-synced while this command ran; its result may mix old and new files" >&2
    fi
    if stage_all "$idx" && after="$(GIT_INDEX_FILE="$idx" git write-tree)"; then
      if [[ ! -e "$state/out/$nonce.cancelled" && "$after" != "$(git rev-parse "$base^{tree}")" ]]; then
        if git diff --binary --full-index "$base" "$after" >"$patch"; then
          bytes="$(wc -c <"$patch" | tr -d ' ')"
        else
          rm -f "$patch"
          echo "boatparty: could not record the command's file changes" >&2
        fi
      fi
    else
      after="-"
      echo "boatparty: could not stage the command's file changes" >&2
    fi
  fi
  unlock_checkout
  rm -f "$state/out/$nonce.cancelled"
  trailer "$nonce" "$rc" "$after" "$bytes"
}

# run <nonce> <pack: 0|1|@name> <wip> <head> <branch or ""> <refs: send|keep> <history: head|full> <command>
#
# Syncs like `sync`, then runs the shell line in the checkout in a session of
# its own with stdin detached, and diffs what it changed.
#
# stdin stays open for as long as the client wants the command: end of file
# (the client closed it, or the client died and its SSH channel closed)
# terminates the command's process group. On a shared SSH master that is the
# only sign of a dead client; the session itself survives it.
mode_run() {
  local nonce="$1" wip="$3" command="$8" rc=0 pid watcher
  find "$state/out" -maxdepth 1 -type f -mmin +1440 -delete 2>/dev/null || true
  receive "$1" "$2" "$3" "$4" "$5" "$6" "$7"
  cd "$repo"
  # SSH sessions default to a soft limit of 1024 open files, which makes
  # file-heavy test suites fail with EMFILE.
  ulimit -n "$(ulimit -Hn)" 2>/dev/null || true
  exec 8<&0
  printf '\n%s!started\n' "$nonce" >&2
  # A session of its own, so a cancel can signal the whole process group.
  perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash -c "$command" </dev/null 8<&- &
  pid=$!
  printf '%s\n' "$pid" >"$state/out/$nonce.pid"
  (
    # Read until end of file; nothing is sent after the pack.
    while IFS= read -r -d '' ignored <&8; do :; done
    : >"$state/out/$nonce.cancelled"
    kill_group "$pid"
  ) </dev/null >/dev/null 2>&1 &
  watcher=$!
  exec 8<&-
  # Quietly: bash would report a job killed by a signal on stderr.
  { wait "$pid"; } 2>/dev/null || rc=$?
  kill "$watcher" 2>/dev/null || true
  wait "$watcher" 2>/dev/null || true
  rm -f "$state/out/$nonce.pid"
  finish "$nonce" "$rc" "$wip"
  exit "$rc"
}

# fetch-patch <nonce>: print the patch a run left, then delete it.
mode_fetch_patch() {
  local file="$state/out/$1.patch"
  cat "$file"
  rm -f "$file"
}

# kill <nonce>: terminate a running command's process group.
mode_kill() {
  local pid
  pid="$(cat "$state/out/$1.pid" 2>/dev/null || true)"
  if [[ -n "$pid" ]]; then kill_group "$pid"; fi
}

case "$mode" in
  sync) mode_sync "$@" ;;
  run) mode_run "$@" ;;
  fetch-patch) mode_fetch_patch "$@" ;;
  kill) mode_kill "$@" ;;
  *)
    echo "boatparty runner: unknown mode $mode" >&2
    exit 2
    ;;
esac
