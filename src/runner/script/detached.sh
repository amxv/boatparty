# Keep run records outside /home/user: boat replaces that mount during a
# restore. /tmp survives a mount swap and a missing record after reboot
# means lost. Each attachment reads both streams by descriptor.
run_directory() {
  local root="${BOATPARTY_RUNNER_JOBS:-/tmp/boatparty-jobs-$UID}"
  [[ "$1" =~ ^[a-zA-Z0-9_-]+$ ]] || exit 2
  mkdir -m 700 "$root" 2>/dev/null || [[ -d "$root" && -O "$root" && ! -L "$root" ]] || exit 2
  job="$root/$1"
}

mode_detach() {
  local nonce="$1" permit line
  run_directory "$nonce"
  load_spec "$8"
  read_credentials
  receive "$1" "$2" "$3" "$4" "$5" "$6" "$7"
  # Only completed jobs are eligible for pruning.
  find "${job%/*}" -mindepth 2 -maxdepth 2 -name ended -mmin +60 -exec sh -c '
    for ended do rm -rf -- "${ended%/ended}"; done' sh {} + 2>/dev/null || true
  (umask 077; mkdir "$job"; touch "$job/out" "$job/err" "$job/lease"; boot_id >"$job/boot")
  # A disconnect before permission is a provably safe retry, including when
  # the remote session survives its mux client.
  printf '\n%s!armed\n' "$nonce" >&2
  IFS= read -r permit && [[ "$permit" == go ]] || { rm -rf "$job"; exit 98; }
  # One private pipe carries the credentials to the detached session.
  BOATPARTY_RUNNER_REPO="$repo" BOATPARTY_CHECKOUT_CHANGED="$CHECKOUT_CHANGED" \
    perl -MPOSIX=:sys_wait_h,setsid -e '
      use strict; use warnings;
      setsid() >= 0 or die;
      my ($dir, $nonce, $window) = splice(@ARGV, 0, 3);
      open(my $owner, ">", "$dir/starting") or die;
      flock($owner, 2) or die;
      rename "$dir/starting", "$dir/supervisor" or die;
      my $pid = fork(); defined $pid or die;
      if (!$pid) { setsid() >= 0 or die; exec @ARGV or die }
      close STDIN;
      my $cancel = 0;
      my $last = 0;
      while (1) {
        if (waitpid($pid, WNOHANG) == $pid) { last }
        if (!$cancel && (-e "$dir/cancel" || time - (stat "$dir/lease")[9] >= $window)) {
          $cancel = 1; $last = time;
          kill("TERM", -$pid) || kill("TERM", $pid);
        }
        if ($cancel && time - $last >= 3) { kill("KILL", -$pid) }
        select undef, undef, undef, 0.1;
      }
      if ($cancel) {
        # Descendants can ignore TERM after the session leader has exited.
        for (1..30) { last unless kill(0, -$pid); select undef, undef, undef, 0.1 }
        kill("KILL", -$pid);
        print STDERR "\n$nonce 143 - 0\n";
      }
      open(my $done, ">", "$dir/ended.tmp") or die;
      print $done (-s "$dir/out"), " ", (-s "$dir/err"), "\n";
      close $done or die;
      rename "$dir/ended.tmp", "$dir/ended" or die;
    ' "$job" "$nonce" "${BOATPARTY_RUNNER_REATTACH_SECS:-600}" bash "$0" supervise "$project" "$@" < <(
      for line in "git-name $cred_name" "git-email $cred_email" "github-token $cred_token"; do
        [[ "$line" == *' ' ]] || printf '%s\n' "$line"
      done
      printf '\n'
    ) >>"$job/out" 2>>"$job/err" &
  stream_job "$nonce" 0 0
}

stream_job() {
  local nonce="$1"
  run_directory "$nonce"
  [[ -f "$job/boot" && "$(cat "$job/boot")" == "$(boot_id)" ]] || exit 98
  # A single framed SSH stdout keeps transport diagnostics separate from
  # command stderr. Only complete records advance a client's two cursors.
  exec perl -e '
    use strict; use warnings;
    my ($dir, @offset) = @ARGV;
    my @files;
    for my $name (qw(out err)) {
      open(my $f, "<", "$dir/$name") or exit 98;
      my $pos = shift @offset;
      exit 98 unless $pos =~ /^\d+$/ && $pos <= -s $f;
      seek($f, $pos, 0) or exit 98;
      push @files, $f;
    }
    $| = 1;
    # The first frame confirms a live attachment, including for a silent job.
    print pack("CN", 3, 0) or exit 255;
    my $last = -1;
    my $opened = time;
    my @limit;
    while (1) {
      if (!@limit && open(my $done, "<", "$dir/ended")) { @limit = split / /, <$done> }
      my $ended = scalar @limit;
      my $busy = 0;
      for my $i (1,0) {
        my $size = 65536;
        if ($ended) {
          my $left = $limit[$i] - sysseek($files[$i], 0, 1);
          next if $left <= 0;
          $size = $left if $left < $size;
        }
        my $n = sysread($files[$i], my $buf, $size);
        exit 98 unless defined $n;
        if ($n) {
          print pack("CN", $i, $n), $buf or exit 255;
          $busy = 1;
        }
      }
      if ($ended && !$busy) { print pack("CN", 2, 0); exit 0 }
      my $now = time;
      if ($last != $now) {
        $last = $now;
        if (!$ended) {
          if (open(my $owner, "<", "$dir/supervisor")) {
            # A released lock on the original file identifies worker loss
            # even if the OS has already reused its PID.
            exit 98 if flock($owner, 1 | 4) && !-e "$dir/ended";
          } elsif ($now - $opened > 5) { exit 98 }
        }
      }
      my $mask = ""; vec($mask, 0, 1) = 1;
      if (select($mask, undef, undef, $busy ? 0 : 0.08) > 0) {
        my $n = sysread(STDIN, my $control, 64);
        exit 255 unless $n;
        if ($control =~ /H/) { utime(undef, undef, "$dir/lease") or exit 98 }
        if ($control =~ /C/) { open(my $c, ">", "$dir/cancel") or die }
      }
    }' "$job" "$2" "$3"
}
