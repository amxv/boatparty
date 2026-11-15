
failed_step=""
preparing_sent=0

# One step's shell line, in the current directory, output to stderr so the
# command's stdout stays its own.
run_step() {
  local section="$1" name="$2" why="$3" line="$4" rc=0 began=$SECONDS
  if [[ "$preparing_sent" == 0 && -n "${nonce:-}" ]]; then
    printf '\n%s!preparing\n' "$nonce" >&2
    preparing_sent=1
  fi
  echo "boatparty: $section $name ($why)" >&2
  # Without the lock's descriptor: a daemon a step starts must not hold it.
  bash -c "$line" </dev/null >&2 9>&- || rc=$?
  if [[ "$rc" != 0 ]]; then
    echo "boatparty: $section step $name failed (exit $rc); the command did not run" >&2
    failed_step="$section.$name"
    return "$rc"
  fi
  echo "boatparty: $section $name done in $((SECONDS - began))s" >&2
}

# A check succeeds quietly.
check_passes() {
  [[ -n "$1" ]] && bash -c "$1" </dev/null >/dev/null 2>&1 9>&-
}

# [[setup]] after every start of the machine: when this boot has not run the
# current setup steps yet, each runs unless its check succeeds.
run_setup() {
  local n=${#bp_setup_names[@]} i marker want why boot
  ((n)) || return 0
  boot="$(boot_id)"
  marker="$state/setup/$project"
  want="$boot $(printf '%s\0' "${bp_setup_names[@]}" "${bp_setup_checks[@]}" "${bp_setup_runs[@]}" |
    git hash-object --stdin)"
  [[ -f "$marker" && "$(cat "$marker")" == "$want" ]] && return 0
  why="the sandbox started"
  if [[ -f "$marker" && "$(cut -d' ' -f1 "$marker")" == "$boot" ]]; then
    why="the setup steps changed"
  fi
  cd "$repo"
  for ((i = 0; i < n; i++)); do
    check_passes "${bp_setup_checks[$i]}" && continue
    run_step setup "${bp_setup_names[$i]}" "$why" "${bp_setup_runs[$i]}" || return $?
    cd "$repo"
  done
  mkdir -p "$state/setup"
  printf '%s\n' "$want" >"$marker"
}

# The key of ensure step $1, in its directory: the step's own line, the
# content of its key files and the output of its key command.
ensure_key() {
  local i="$1" file
  {
    printf 'run %s\n' "${bp_ensure_runs[$i]}"
    while IFS= read -r file; do
      [[ -z "$file" ]] && continue
      if [[ -f "$file" ]]; then
        printf 'file %s %s\n' "$file" "$(git hash-object --no-filters -- "$file")"
      else
        printf 'file %s -\n' "$file"
      fi
    done <<<"${bp_ensure_keys[$i]}"
    if [[ -n "${bp_ensure_key_commands[$i]}" ]]; then
      printf 'command\n'
      bash -c "${bp_ensure_key_commands[$i]}" </dev/null 2>&1 9>&- || printf 'exit %s\n' "$?"
    fi
  } | git hash-object --stdin
}

# [[ensure]] before a command: a step runs when its key changed since it
# last succeeded, or when its check fails. A step whose directory does not
# exist is skipped.
run_ensure() {
  local n=${#bp_ensure_names[@]} i name dir key keyfile why rc
  ((n)) || return 0
  mkdir -p "$state/ensure/$project"
  for ((i = 0; i < n; i++)); do
    name="${bp_ensure_names[$i]}"
    dir="$repo"
    [[ -n "${bp_ensure_dirs[$i]}" ]] && dir="$repo/${bp_ensure_dirs[$i]}"
    [[ -d "$dir" ]] || continue
    cd "$dir"
    key="$(ensure_key "$i")"
    keyfile="$state/ensure/$project/$name"
    why=""
    if [[ ! -f "$keyfile" ]]; then
      why="first run"
    elif [[ "$(cat "$keyfile")" != "$key" ]]; then
      why="its key changed"
    elif [[ -n "${bp_ensure_checks[$i]}" ]] && ! check_passes "${bp_ensure_checks[$i]}"; then
      why="its check failed"
    fi
    if [[ -n "$why" ]]; then
      rc=0
      run_step ensure "$name" "$why" "${bp_ensure_runs[$i]}" || rc=$?
      if [[ "$rc" != 0 ]]; then
        cd "$repo"
        return "$rc"
      fi
      # Keyed on what is there now, so a step that rewrites a key file
      # settles instead of running again next time.
      cd "$dir"
      ensure_key "$i" >"$keyfile"
    fi
  done
  cd "$repo"
}

# Everything that prepares the checkout for a command, under the checkout
# lock so overlapping commands do not install at once.
prepare_command() {
  local rc=0
  lock_checkout
  heal_caches || echo "boatparty: checking the build caches failed" >&2
  run_setup || rc=$?
  if [[ "$rc" == 0 ]]; then run_ensure || rc=$?; fi
  unlock_checkout
  cd "$repo"
  return "$rc"
}

