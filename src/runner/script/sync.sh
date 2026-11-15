# Moves the working tree to the snapshot tree. Only paths whose content
# differs are written or deleted, so unchanged files keep their mtimes. The
# real index ends up matching HEAD, so `git status` equals the client's.
# Sets CHECKOUT_CHANGED=1 when any file was written or removed.
CHECKOUT_CHANGED=0
apply_snapshot() {
  local wip="$1" head="$2" branch="$3" refs_file="$4" idx before wip_tree
  idx="$(sync_index)"
  cd "$repo"
  if [[ -n "$refs_file" ]]; then
    mirror_refs "$branch" "$refs_file"
  fi
  git update-ref refs/boatparty/wip "$wip"
  git update-ref refs/boatparty/head "$head"
  stage_all "$idx"
  before="$(GIT_INDEX_FILE="$idx" git write-tree)"
  GIT_INDEX_FILE="$idx" git read-tree --reset -u "$wip"
  wip_tree="$(git rev-parse "$wip^{tree}")"
  if [[ "$before" != "$wip_tree" ]]; then
    CHECKOUT_CHANGED=1
    # Changed ignore rules can expose files the old rules hid (another
    # checkout's build output when a member moves between repositories, or
    # a pattern the client dropped). They are not in the snapshot, so they go
    # like any other file the client lacks; left in place, the next command
    # would send them back as its own changes.
    if git diff --name-only "$before" "$wip_tree" | grep -q '\(^\|/\)\.gitignore$'; then
      GIT_INDEX_FILE="$idx" git clean -fdq
    fi
  fi
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
  # A seed fetch rewrites .git/shallow under the same lock.
  lock_checkout
  case "$history" in
    head) mark_shallow "$head" ;;
    full) deepen ;;
    *) echo "boatparty runner: unknown history mode $history" >&2; exit 2 ;;
  esac
  unlock_checkout
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
  restore_dir_mtimes
  apply_snapshot "$wip" "$head" "$branch" "$refs_file"
  save_dir_mtimes || echo "boatparty: recording directory mtimes failed" >&2
  unlock_checkout
  [[ -n "$refs_file" ]] && rm -f "$refs_file"
  return 0
}

# sync <nonce> <pack: 0|1|@name> <wip> <head> <branch or ""> <refs: send|keep> <history: head|full> [<spec>]
#
# A bare sync runs no steps and floors nothing.
mode_sync() {
  local nonce="$1" wip="$3"
  load_spec "${8:--}"
  receive "$1" "$2" "$3" "$4" "$5" "$6" "$7"
  trailer "$nonce" 0 "$(git -C "$repo" rev-parse "$wip^{tree}")" 0
  maybe_maintain
}

# seed <nonce> <history: head|full>
#
# Fetches commits the client knows the project's GitHub remote has, so the
# pack that follows needs to carry only what the remote lacks. stdin holds
# "url <url>", "timeout <seconds>", one "want <sha>" per commit, optionally
# "token <token>", then an empty line. Prints "seeded" or "failed <reason>"
# on stdout and exits 0 either way; a failed seed only means a bigger pack.
#
# The fetch goes to the URL itself, never a configured remote, so nothing is
# written to .git/config. A token stays in this shell's memory and reaches
# Git through a pipe on fd 7, read by a one-shot credential helper: it is
# never in an argument list, the environment, the URL or a file. Every other
# credential helper (a "store" helper would save it) is switched off.
mode_seed() {
  local history="$2" line url="" timeout=180 token="" err rc=0 sha
  local wants=() refspecs=() depth=() helper
  while IFS= read -r line; do
    [[ -z "$line" ]] && break
    case "$line" in
      "url "*) url="${line#url }" ;;
      "timeout "*) timeout="${line#timeout }" ;;
      "want "*) wants+=("${line#want }") ;;
      "token "*) token="${line#token }" ;;
    esac
  done
  case "$url" in
    https://github.com/*) ;;
    file:///*) if [[ "${BOATPARTY_RUNNER_SEED_FILE:-}" != 1 ]]; then url=""; fi ;;
    *) url="" ;;
  esac
  if [[ -z "$url" ]]; then echo "failed not a GitHub URL"; return 0; fi
  if [[ ${#wants[@]} -eq 0 ]]; then echo "failed nothing to fetch"; return 0; fi
  case "$timeout" in "" | *[!0-9]*) timeout=180 ;; esac
  for sha in "${wants[@]}"; do
    case "$sha" in
      *[!0-9a-f]* | "") echo "failed bad commit name"; return 0 ;;
    esac
    refspecs+=("+$sha:refs/boatparty/seed/$sha")
  done
  ensure_repo
  case "$history" in
    head) depth=(--depth=1) ;;
    full) if [[ -f "$repo/.git/shallow" ]]; then depth=(--unshallow); fi ;;
    *) echo "boatparty runner: unknown history mode $history" >&2; exit 2 ;;
  esac
  # shellcheck disable=SC2016
  helper='!f() { test "$1" = get || exit 0; IFS= read -r t <&7 || exit 0; test -n "$t" || exit 0; echo username=x-access-token; echo "password=$t"; }; f'
  lock_checkout
  git -C "$repo" for-each-ref --format='delete %(refname)' refs/boatparty/seed |
    git -C "$repo" update-ref --stdin
  err="$(
    GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= SSH_ASKPASS= GCM_INTERACTIVE=never \
      GIT_HTTP_LOW_SPEED_LIMIT=1024 GIT_HTTP_LOW_SPEED_TIME=30 \
      perl -e 'alarm shift; exec @ARGV or exit 127' "$timeout" \
      git -C "$repo" -c protocol.version=2 -c credential.helper= \
      -c "credential.helper=$helper" -c core.askPass= \
      fetch --quiet --no-tags --no-recurse-submodules --no-write-fetch-head \
      ${depth[@]+"${depth[@]}"} "$url" "${refspecs[@]}" \
      7< <(printf '%s\n' "$token") 2>&1 >/dev/null
  )" || rc=$?
  token=""
  if [[ "$rc" == 0 ]]; then
    for sha in "${wants[@]}"; do
      git -C "$repo" cat-file -e "$sha^{commit}" 2>/dev/null || rc=1
    done
    [[ "$rc" == 0 ]] || err="the fetch did not bring every commit"
  fi
  unlock_checkout
  if [[ "$rc" == 0 ]]; then
    echo seeded
  elif [[ "$rc" == 142 ]]; then
    echo "failed the fetch took longer than ${timeout}s"
  else
    err="$(printf '%s\n' "$err" | awk '/^(fatal|error): / && r == "" { r = $0 } NF { last = $0 } END { print (r != "" ? r : last) }')"
    echo "failed ${err:-git fetch exited $rc}"
  fi
}

# Keeps the repository's pack and loose-object count bounded: every sync
# adds a pack and every command's changes add loose objects. When either
# passes its limit, a detached `maintain` runs, so nothing waits for it and
# no lock is held across it.
maybe_maintain() {
  local packs loose
  [[ -d "$repo/.git/objects/pack" ]] || return 0
  packs="$(find "$repo/.git/objects/pack" -maxdepth 1 -name '*.pack' 2>/dev/null | wc -l | tr -d ' ')"
  loose="$(git -C "$repo" count-objects 2>/dev/null | awk '{ print $1 }')"
  if [[ "$packs" -gt "${BOATPARTY_RUNNER_MAX_PACKS:-8}" || "${loose:-0}" -gt "${BOATPARTY_RUNNER_MAX_LOOSE:-2000}" ]]; then
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash "$0" maintain "$project" \
      </dev/null >/dev/null 2>&1 9>&- &
  fi
  return 0
}

# maintain: roll small packs and loose objects into bigger packs (a
# geometric repack). It never drops an object, so the shallow boundaries
# and every object the client counts on stay intact. One at a time.
mode_maintain() {
  [[ -d "$repo/.git" ]] || return 0
  exec 6>"$state/maintain.lock"
  if command -v flock >/dev/null 2>&1; then flock -n 6 || return 0; fi
  nice -n 10 git -C "$repo" repack -d -q --geometric=2 || true
  date +%s >"$state/maintained"
}

