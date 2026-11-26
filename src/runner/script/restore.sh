
# ---------------------------------------------------------------------------
# The project's settings (the RunnerSpec) and the workarounds for boat's lazy
# disk restore. Nothing here names a language or a tool: what differs between
# stacks comes from the spec.

# Defaults: what a spec of "-" applies.
bp_ram_tmp=0
bp_floor=0
bp_cache_days=3
bp_min_free_gb=25
bp_desktop_keep=0
bp_output_dirs=()
bp_cache_dirs=()
bp_env=()
bp_setup_names=()
bp_setup_checks=()
bp_setup_runs=()
bp_ensure_names=()
bp_ensure_dirs=()
bp_ensure_requires=()
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
    cd -P "$repo" || exit 0
    shopt -s nullglob
    IFS=''
    local pattern path
    for pattern in "${bp_output_dirs[@]}"; do
      # shellcheck disable=SC2086 # the pattern is globbed on purpose
      for path in $pattern; do
        path="${path#./}"
        path="${path%/}"
        if [[ -d "$path" && ! -L "$path" && "$path" != *$'\n'*  ]] &&
           (cd -L -- "$path" && [[ "$PWD" == "$(pwd -P)" ]]); then printf '%s\n' "$path"; fi
      done
    done
  )
}

floor_on() { [[ "$bp_floor" == 1 && ${#bp_output_dirs[@]} -gt 0 ]]; }

# A long-running server keeps building; flooring waits until it stops.
# (A server records its pid in out/<nonce>.serve while it runs; see
# "Servers".)
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
