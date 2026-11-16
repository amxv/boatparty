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
#
# After a command that ran, build outputs are floored; directory mtimes are
# recorded either way. Both are best-effort.
finish() {
  local nonce="$1" rc="$2" wip="$3" ran="${4:-1}" idx base after="-" bytes=0 patch
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
  if [[ "$ran" == 1 ]]; then
    floor_mtimes || echo "boatparty: flooring build output mtimes failed" >&2
  fi
  save_dir_mtimes || echo "boatparty: recording directory mtimes failed" >&2
  unlock_checkout
  rm -f "$state/out/$nonce.cancelled"
  trailer "$nonce" "$rc" "$after" "$bytes"
}

# run <nonce> <pack: 0|1|@name> <wip> <head> <branch or ""> <refs: send|keep> <history: head|full> <spec> <command>
#
# stdin starts with the build credentials block ("git-name <name>",
# "git-email <email>", "github-token <token>", each optional, then an empty
# line), followed by what `sync` reads.
#
# Syncs like `sync`; applies the credentials (see "Build credentials") for
# the steps and the command; exports [run.env] and BOATPARTY=1; runs the setup steps
# (first command after a start) and the ensure steps, announcing them with
# the event "preparing"; then runs the shell line in the checkout in a
# session of its own with stdin detached and a private RAM TMPDIR, and diffs
# what it changed. A failing step ends the invocation with the step's exit
# code and the event "failed-step <section>.<name>"; the command does not
# run.
#
# stdin stays open for as long as the client wants the command: end of file
# (the client closed it, or the client died and its SSH channel closed)
# terminates the command's process group. On a shared SSH master that is the
# only sign of a dead client; the session itself survives it.
#
# serve <nonce> ... <command> <ready-port or ->: the same for a server (see
# "Servers"): it is marked as serving while it runs, and with a ready port the
# event "ready" follows once that port listens.
mode_run() {
  local nonce="$1" wip="$3" command="$9" ready_port="${10:--}" rc=0 pid watcher ready=""
  local shm run_tmp="" old_tmp
  if [[ "$serve_mode" == 1 && "$ready_port" != - && ! "$ready_port" =~ ^[0-9]+$ ]]; then
    echo "boatparty runner: bad ready port $ready_port" >&2
    exit 2
  fi
  load_spec "$8"
  trap stop_token EXIT
  read_credentials
  # A server may outlive a day; its marker stays while it runs.
  find "$state/out" -maxdepth 1 -type f ! -name '*.serve' -mmin +1440 -delete 2>/dev/null || true
  prune_serve_markers
  receive "$1" "$2" "$3" "$4" "$5" "$6" "$7"
  cd "$repo"
  apply_identity
  apply_env
  serve_token
  prepare_command || rc=$?
  if [[ "$rc" != 0 ]]; then
    stop_token
    printf '\n%s!failed-step %s\n' "$nonce" "${failed_step:-unknown}" >&2
    finish "$nonce" "$rc" "$wip" 0
    exit "$rc"
  fi
  if [[ "$CHECKOUT_CHANGED" == 1 ]] && floor_on; then wait_next_second; fi
  # Restored machines pay a lot per fsync, which makes scratch-heavy tests
  # slow; a private RAM TMPDIR keeps scratch I/O off the disk.
  shm="${BOATPARTY_RUNNER_SHM:-/dev/shm}"
  if [[ "$bp_ram_tmp" == 1 && -d "$shm" && -w "$shm" ]]; then
    find "$shm" -maxdepth 1 -name 'boatparty-*' -mmin +1440 -exec rm -rf {} + 2>/dev/null || true
    run_tmp="$shm/boatparty-$nonce"
    old_tmp="${TMPDIR-}"
    mkdir -m 700 "$run_tmp" 2>/dev/null || mkdir -p "$run_tmp"
    export TMPDIR="$run_tmp"
  fi
  # SSH sessions default to a soft limit of 1024 open files, which makes
  # file-heavy test suites fail with EMFILE.
  ulimit -n "$(ulimit -Hn)" 2>/dev/null || true
  exec 8<&0
  printf '\n%s!started\n' "$nonce" >&2
  # A session of its own, so a cancel can signal the whole process group.
  perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash -c "$command" </dev/null 8<&- &
  pid=$!
  printf '%s\n' "$pid" >"$state/out/$nonce.pid"
  if [[ "$serve_mode" == 1 ]]; then
    printf '%s\n' "$pid" >"$state/out/$nonce.serve"
    if [[ "$ready_port" != - ]]; then
      watch_ready "$nonce" "$ready_port" "$pid" </dev/null >/dev/null 8<&- &
      ready=$!
    fi
  fi
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
  if [[ -e "$state/out/$nonce.cancelled" ]]; then
    # It is stopping the group: let it finish, so whatever ignored TERM in
    # the group still gets KILL.
    wait "$watcher" 2>/dev/null || true
  else
    kill "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
  fi
  if [[ -n "$ready" ]]; then
    kill "$ready" 2>/dev/null || true
    wait "$ready" 2>/dev/null || true
  fi
  stop_token
  rm -f "$state/out/$nonce.pid" "$state/out/$nonce.serve"
  if [[ -n "$run_tmp" ]]; then
    chmod -R u+rwx "$run_tmp" 2>/dev/null || true
    rm -rf "$run_tmp"
    if [[ -n "$old_tmp" ]]; then export TMPDIR="$old_tmp"; else unset TMPDIR; fi
  fi
  finish "$nonce" "$rc" "$wip" 1
  maybe_maintain
  exit "$rc"
}

# fetch-patch <nonce> [discard]: print the patch a run left, then delete
# it. With "discard" (the client downloaded it another way), only delete it.
mode_fetch_patch() {
  local file="$state/out/$1.patch"
  if [[ "${2:-}" != discard ]]; then cat "$file"; fi
  rm -f "$file"
}

# kill <nonce>: terminate a running command's process group.
mode_kill() {
  local pid
  pid="$(cat "$state/out/$1.pid" 2>/dev/null || true)"
  if [[ -n "$pid" ]]; then kill_group "$pid"; fi
}

# settle <spec>: the last step on a builder before its disk is saved as a
# base. Floors every build output (not only those changed since the last
# pass), records directory mtimes, drops this tool's per-invocation scratch
# (out/, in/), packs the repository's reachable objects into one pack, and
# flushes the filesystem, so the stop that follows captures a quiet disk.
# Prints "settled".
mode_settle() {
  load_spec "${1:--}"
  lock_checkout
  rm -f "$state/floor/$project.stamp"
  floor_mtimes || echo "boatparty: flooring build output mtimes failed" >&2
  save_dir_mtimes || echo "boatparty: recording directory mtimes failed" >&2
  find "$state/out" "$state/in" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true
  if [[ -d "$repo/.git" ]]; then
    # After any background `maintain` (its lock), never beside it.
    exec 6>"$state/maintain.lock"
    if command -v flock >/dev/null 2>&1; then flock 6; fi
    git -C "$repo" repack -a -d -q 2>/dev/null || echo "boatparty: packing the repository failed" >&2
    exec 6>&-
  fi
  unlock_checkout
  sync
  wait_next_second
  echo settled
}

case "$mode" in
  sync) mode_sync "$@" ;;
  run) mode_run "$@" ;;
  serve)
    serve_mode=1
    mode_run "$@"
    ;;
  pipe) mode_pipe "$@" ;;
  seed) mode_seed "$@" ;;
  credential) mode_credential "$@" ;;
  hold) mode_hold "$@" ;;
  shell) mode_board "$@" ;;
  maintain) mode_maintain ;;
  fetch-patch) mode_fetch_patch "$@" ;;
  kill) mode_kill "$@" ;;
  settle) mode_settle "$@" ;;
  *)
    echo "boatparty runner: unknown mode $mode" >&2
    exit 2
    ;;
esac
