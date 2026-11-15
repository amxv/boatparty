#!/usr/bin/env bash
# The boatparty runner: the remote half of boatparty.
#
# The client embeds this file, uploads it to the sandbox as
# /home/user/.boatparty/bin/runner-<content-hash>.sh and runs it by path, so
# the sandbox always runs the matching version and no process command line
# carries the script text.
#
# Invocation: bash runner-<hash>.sh <mode> <project> <args...>
# Modes: sync, run, seed, fetch-patch, kill, maintain.
#
# sync and run name a RunnerSpec: the project's resolved settings, written by
# the client to /home/user/.boatparty/specs/<hash>.sh (also named by content
# hash) and sourced here. A spec file that is missing is reported like a
# missing runner (exit 96), so the client uploads both and retries. "-"
# names no spec: nothing is applied. The spec only holds data (shell arrays);
# every workaround below is either driven by it or stack-neutral.
#
# Results travel on stderr as frames keyed by a per-invocation nonce:
#   event:                "\n<nonce>!<event>\n" (run emits "started")
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

# ---------------------------------------------------------------------------
# The project's settings (the RunnerSpec) and the workarounds for boat's lazy
# disk restore. Nothing here names a language or a tool: what differs between
# stacks comes from the spec.

# Defaults: what a spec of "-" applies.
bp_ram_tmp=0
bp_floor=0
bp_output_dirs=()
bp_cache_dirs=()
bp_env=()
bp_setup_names=()
bp_setup_checks=()
bp_setup_runs=()
bp_ensure_names=()
bp_ensure_dirs=()
bp_ensure_keys=()
bp_ensure_key_commands=()
bp_ensure_checks=()
bp_ensure_runs=()

# Load the spec an invocation names. A missing spec is reported exactly like
# a missing runner, before anything is read from stdin.
load_spec() {
  local hash="${1:--}"
  case "$hash" in
    -) return 0 ;;
    *[!0-9a-f]*) echo "boatparty runner: bad spec name $hash" >&2; exit 2 ;;
  esac
  if [[ ! -f "$state/specs/$hash.sh" ]]; then
    echo boatparty-runner-missing >&2
    exit 96
  fi
  # shellcheck source=/dev/null
  . "$state/specs/$hash.sh"
}

# This boot of the machine. A create or a resume onto a new machine is a new
# boot; a resume that reclaims the kept machine is not, and kept everything.
boot_id() {
  if [[ -n "${BOATPARTY_RUNNER_BOOT_ID:-}" ]]; then
    printf '%s' "$BOATPARTY_RUNNER_BOOT_ID"
  elif [[ -r /proc/sys/kernel/random/boot_id ]]; then
    cat /proc/sys/kernel/random/boot_id
  else
    printf unknown
  fi
}

# boat restores a stopped sandbox onto a new machine lazily: the home
# directory is served by a FUSE filesystem until the download ends.
is_hydrating() {
  case "${BOATPARTY_RUNNER_HYDRATING:-}" in
    1) return 0 ;;
    0) return 1 ;;
  esac
  command -v findmnt >/dev/null 2>&1 || return 1
  case "$(findmnt -n -o FSTYPE -T "$repo" 2>/dev/null || true)" in
    fuse*) return 0 ;;
  esac
  return 1
}

# [run.env] in order, then BOATPARTY=1. Values are expanded by bash in the
# checkout, as inside double quotes ($HOME, $PATH, $PWD).
apply_env() {
  local i name value
  for ((i = 0; i + 1 < ${#bp_env[@]}; i += 2)); do
    name="${bp_env[$i]}"
    value="${bp_env[$((i + 1))]}"
    if [[ ! "$name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
      echo "boatparty runner: bad variable name $name" >&2
      exit 2
    fi
    value="${value//\"/\\\"}"
    # A value may name a variable that is not set; that expands to nothing.
    set +u
    eval "export $name=\"$value\""
    set -u
  done
  export BOATPARTY=1
}

# The output directories that exist, relative to the checkout, one per line.
# Patterns may hold globs.
output_roots() {
  ((${#bp_output_dirs[@]})) || return 0
  (
    cd "$repo" || exit 0
    shopt -s nullglob
    IFS=''
    local pattern path
    for pattern in "${bp_output_dirs[@]}"; do
      # shellcheck disable=SC2086 # the pattern is globbed on purpose
      for path in $pattern; do
        path="${path#./}"
        path="${path%/}"
        if [[ -d "$path" && ! -L "$path" ]]; then printf '%s\n' "$path"; fi
      done
    done
  )
}

floor_on() { [[ "$bp_floor" == 1 && ${#bp_output_dirs[@]} -gt 0 ]]; }

# A long-running server keeps building; flooring waits until it stops.
# (Servers record out/<nonce>.serve; nothing writes one yet.)
serve_active() {
  local file
  for file in "$state"/out/*.serve; do
    [[ -f "$file" ]] && kill -0 "$(cat "$file")" 2>/dev/null && return 0
  done
  return 1
}

# find(1) arguments that skip the spec's cache directories.
prune_caches() {
  local name first=1
  ((${#bp_cache_dirs[@]})) || return 0
  printf '%s\n' '('
  for name in "${bp_cache_dirs[@]}"; do
    if [[ "$first" == 0 ]]; then printf '%s\n' -o; fi
    printf '%s\n' -name "$name"
    first=0
  done
  printf '%s\n' ')' -prune -o
}

# boat restores some file mtimes truncated to whole seconds, which makes a
# build output look a fraction of a second older than what it was built
# from. Flooring build outputs ourselves after every command makes that
# truncation a no-op (build tools count equal times as fresh). Only files
# under the output directories changed since the last pass are touched;
# never a file of the synced tree (a source), never a cache directory (those
# are content-addressed), and nothing while a server is running.
floor_mtimes() {
  floor_on || return 0
  serve_active && return 0
  local dir="$state/floor" stamp next sources roots=() prune=() newer=() line
  stamp="$dir/$project.stamp"
  next="$stamp.next"
  sources="$dir/$project.sources"
  mkdir -p "$dir"
  while IFS= read -r line; do roots+=("$line"); done < <(output_roots)
  while IFS= read -r line; do prune+=("$line"); done < <(prune_caches)
  touch "$next"
  if ((${#roots[@]})); then
    [[ -f "$stamp" ]] && newer=(-newer "$stamp")
    GIT_INDEX_FILE="$(sync_index)" git -C "$repo" ls-files -z -- "${roots[@]}" >"$sources" 2>/dev/null || : >"$sources"
    # Files vanish mid-walk when a compiler prunes its caches; find then
    # exits non-zero.
    { (cd "$repo" && find "${roots[@]}" -xdev ${prune[@]+"${prune[@]}"} \
      ${newer[@]+"${newer[@]}"} -print0 2>/dev/null) || true; } |
      (cd "$repo" && perl -0 -e '
        my %source;
        if (open(my $list, "<", shift)) { local $/ = "\0"; while (<$list>) { chomp; $source{$_} = 1 } }
        local $/ = "\0";
        while (my $path = <STDIN>) {
          chomp $path;
          next if $source{$path} || -l $path;
          my @s = stat($path) or next;
          utime($s[8], $s[9], $path);
        }' "$sources")
    rm -f "$sources"
  fi
  mv "$next" "$stamp"
}

# A command starting in the same second as a sync that wrote files would
# make those files look as old as outputs it then builds, once the outputs
# are floored; waiting for the next whole second avoids a later spurious
# rebuild.
wait_next_second() {
  perl -MTime::HiRes=time,sleep -e 'my $t = time; sleep(int($t) + 1.01 - $t)'
}

# While hydrating, directory mtimes read as the restore time and keep moving
# as files land, so build tools that watch a directory rebuild. Record the
# mtimes of the synced tree's directories after every sync and command, and
# put them back before a sync only while the member is still hydrating.
# Afterwards directory mtimes are real and syncs change them legitimately.
save_dir_mtimes() {
  local idx dir="$state/dirs"
  idx="$(sync_index)"
  [[ -f "$idx" ]] || return 0
  mkdir -p "$dir"
  GIT_INDEX_FILE="$idx" git -C "$repo" ls-files -z |
    (cd "$repo" && perl -0 -ne '
      chomp; my $d = $_;
      while ($d =~ s{/[^/]*\z}{}) { $dirs{$d} = 1 }
      END {
        $dirs{"."} = 1;
        for my $path (sort keys %dirs) {
          my @s = lstat($path) or next;
          next unless -d _;
          print "$s[9]\t$path\0";
        }
      }') >"$dir/$project.next"
  mv "$dir/$project.next" "$dir/$project"
}

restore_dir_mtimes() {
  local file="$state/dirs/$project"
  [[ -f "$file" ]] || return 0
  is_hydrating || return 0
  (cd "$repo" && perl -0 -ne '
    chomp; my ($t, $path) = split /\t/, $_, 2;
    next unless defined $path;
    my @s = lstat($path) or next;
    next unless -d _;
    utime($s[8], $t, $path) if $s[9] != $t;' "$file") || true
}

# While the home directory is served by the lazy restore filesystem, files
# in a compiler's cache directory have turned up listed but impossible to
# open (ENOENT), including files written during the same boot, and the
# compiler then fails instead of rebuilding. Inside each of the spec's cache
# directories under the output directories, every entry holding a file that
# cannot be opened is dropped whole; the compiler rebuilds it. The walk only
# opens files (about 0.25 s for 2,600 files on that filesystem). It runs
# before every command while hydrating, and once per boot after.
heal_caches() {
  ((${#bp_cache_dirs[@]})) || return 0
  local marker="$state/healed/$project" want="" roots=() names=() line name
  if ! is_hydrating; then
    want="$(boot_id) settled"
    [[ -f "$marker" && "$(cat "$marker")" == "$want" ]] && return 0
  fi
  while IFS= read -r line; do roots+=("$line"); done < <(output_roots)
  if ((${#roots[@]})); then
    for name in "${bp_cache_dirs[@]}"; do names+=(-o -name "$name"); done
    { (cd "$repo" && find "${roots[@]}" -xdev -type d \( "${names[@]:1}" \) -prune -print0 2>/dev/null) || true; } |
      (cd "$repo" && perl -0 -MFile::Find -e '
        local $/ = "\0";
        while (my $cache = <STDIN>) {
          chomp $cache;
          opendir(my $dh, $cache) or next;
          my @entries = grep { !/\A\.\.?\z/ } readdir $dh;
          closedir $dh;
          for my $entry (@entries) {
            my $path = "$cache/$entry";
            my ($bad, $why);
            my $probe = sub {
              my $file = shift;
              return if $bad || -l $file || -d $file;
              if (open(my $fh, "<", $file)) { close $fh } else { ($bad, $why) = ($file, "$!") }
            };
            if (-d $path && !-l $path) {
              find({ no_chdir => 1, wanted => sub { $probe->($_) } }, $path);
            } else {
              $probe->($path);
            }
            next unless $bad;
            print STDERR "boatparty: dropping $path: a restore left $bad unreadable ($why)\n";
            system("rm", "-rf", "--", $path);
          }
        }')
  fi
  if [[ -n "$want" ]]; then
    mkdir -p "$state/healed"
    printf '%s\n' "$want" >"$marker"
  fi
}

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
# Syncs like `sync`; exports [run.env] and BOATPARTY=1; runs the setup steps
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
mode_run() {
  local nonce="$1" wip="$3" command="$9" rc=0 pid watcher shm run_tmp="" old_tmp
  load_spec "$8"
  find "$state/out" -maxdepth 1 -type f -mmin +1440 -delete 2>/dev/null || true
  receive "$1" "$2" "$3" "$4" "$5" "$6" "$7"
  cd "$repo"
  apply_env
  prepare_command || rc=$?
  if [[ "$rc" != 0 ]]; then
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
  if [[ -n "$run_tmp" ]]; then
    chmod -R u+rwx "$run_tmp" 2>/dev/null || true
    rm -rf "$run_tmp"
    if [[ -n "$old_tmp" ]]; then export TMPDIR="$old_tmp"; else unset TMPDIR; fi
  fi
  finish "$nonce" "$rc" "$wip" 1
  maybe_maintain
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
  seed) mode_seed "$@" ;;
  maintain) mode_maintain ;;
  fetch-patch) mode_fetch_patch "$@" ;;
  kill) mode_kill "$@" ;;
  *)
    echo "boatparty runner: unknown mode $mode" >&2
    exit 2
    ;;
esac
