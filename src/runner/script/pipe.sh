# ---------------------------------------------------------------------------
# Piped input (`pipe` mode, `boatparty pipe`).
#
# pipe <nonce> <spec> <command>
#
# stdin starts with the build credentials block (as for `run`), then carries
# the caller's input as frames: a 4-byte big-endian length, then that many
# bytes. A frame of length 0 ends the input. The command reads the decoded
# bytes on its stdin, so the input never appears in an argument list, a file
# or a log. Nothing is synced first and no step runs; the credentials,
# [run.env] and BOATPARTY=1 apply as for `run`. The command runs in the
# checkout, in a session of its own.
#
# The stream stays the liveness signal, as for `run`: after the input ends
# the client keeps it open until the trailer, so end of file (the client
# closed it, or died and its SSH channel closed) terminates the command's
# process group at any point, whether or not the input had ended. Without
# the framing, end of input and a dead client would look the same.
#
# The trailer has no tree and no patch: "\n<nonce> <exit-code> - 0\n".

# Copies the frames on stdin to stdout until end of file. After the
# end-of-input frame, or once the command stops reading, stdout is closed and
# the rest is read and dropped. perl, so arbitrary bytes pass unchanged.
#
# Run it only as a background job: it execs perl, so no shell is left
# holding the FIFO open after perl closes it (the command would then never
# see the end of its input).
pipe_decode() {
  exec perl -e '
    use strict;
    binmode STDIN;
    binmode STDOUT;
    $SIG{PIPE} = "IGNORE";
    my $open = 1;
    sub take {
      my ($n) = @_;
      my $buf = "";
      while (length($buf) < $n) {
        my $got = sysread(STDIN, $buf, $n - length($buf), length($buf));
        if (!defined $got) { next if $!{EINTR}; return undef }
        return undef if $got == 0;
      }
      return $buf;
    }
    while (defined(my $head = take(4))) {
      my $n = unpack("N", $head);
      if ($n == 0) { close STDOUT if $open; $open = 0; next }
      my $data = take($n);
      last unless defined $data;
      next unless $open;
      my $off = 0;
      while ($off < $n) {
        my $wrote = syswrite(STDOUT, $data, $n - $off, $off);
        if (!defined $wrote) {
          next if $!{EINTR};
          close STDOUT;
          $open = 0;
          last;
        }
        $off += $wrote;
      }
    }'
}

mode_pipe() {
  local nonce="${1:-}" command="${3:-}" rc=0 pid watcher fifo
  if [[ ! "$nonce" =~ ^[0-9a-f]+$ ]]; then
    echo "boatparty runner: bad nonce $nonce" >&2
    exit 2
  fi
  load_spec "${2:--}"
  trap stop_token EXIT
  read_credentials
  mkdir -p "$repo"
  cd "$repo"
  apply_identity
  apply_env
  serve_token
  ulimit -n "$(ulimit -Hn)" 2>/dev/null || true
  fifo="$state/out/$nonce.in"
  rm -f "$fifo"
  mkfifo -m 600 "$fifo"
  exec 8<&0
  printf '\n%s!started\n' "$nonce" >&2
  # A session of its own, so a cancel can signal the whole process group.
  perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash -c "$command" <"$fifo" 8<&- &
  pid=$!
  printf '%s\n' "$pid" >"$state/out/$nonce.pid"
  (
    # Decode until the stream ends; then the client is gone or cancelled.
    decoder=""
    trap 'if [[ -n "$decoder" ]]; then kill "$decoder" 2>/dev/null; fi; exit 0' TERM
    pipe_decode <&8 >"$fifo" &
    decoder=$!
    wait "$decoder" 2>/dev/null || true
    decoder=""
    : >"$state/out/$nonce.cancelled"
    kill_group "$pid"
  ) </dev/null >/dev/null 2>&1 &
  watcher=$!
  exec 8<&-
  { wait "$pid"; } 2>/dev/null || rc=$?
  if [[ -e "$state/out/$nonce.cancelled" ]]; then
    wait "$watcher" 2>/dev/null || true
  else
    kill "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
  fi
  stop_token
  rm -f "$state/out/$nonce.pid" "$state/out/$nonce.cancelled" "$fifo"
  trailer "$nonce" "$rc" - 0
  exit "$rc"
}
