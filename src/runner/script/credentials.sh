# ---------------------------------------------------------------------------
# Build credentials. `run` reads them from the head of its stdin; nothing is
# written to the sandbox's disk, and nothing outlives the command.
#
# The Git identity (not secret) goes to the command's environment
# (GIT_AUTHOR_*, GIT_COMMITTER_*) and, through GIT_CONFIG_COUNT, to Git's
# user.name and user.email; [run.env] can still override it.
#
# A GitHub token stays in this shell only until a small server holds it:
# perl, listening on a Unix socket in a fresh 0700 directory in RAM, fed the
# token through a pipe. Git in the command's tree reaches it through a
# credential helper (this script's `credential` mode) configured in the
# environment (GIT_CONFIG_COUNT) for https://github.com only, after
# switching off every other helper for that URL (a `store` or `cache` helper
# would keep the token). The helper answers only `get` for that protocol
# and host. The environment holds the socket's path, never the token, and no
# argument list holds it. When the command ends the server is stopped and
# its directory removed; the server also quits on its own if this runner
# dies.

cred_name=""
cred_email=""
cred_token=""
cred_dir=""
cred_pid=""
# The URL the token is for. A test knob points it at a local stand-in.
cred_url="${BOATPARTY_RUNNER_CREDENTIAL_URL:-https://github.com}"

# The block at the head of run's stdin: "key value" lines, then an empty line.
read_credentials() {
  local line
  while IFS= read -r line; do
    [[ -z "$line" ]] && break
    case "$line" in
      "git-name "*) cred_name="${line#git-name }" ;;
      "git-email "*) cred_email="${line#git-email }" ;;
      "github-token "*) cred_token="${line#github-token }" ;;
    esac
  done
}

# Appends one entry to the configuration Git reads from the environment.
add_git_config() {
  local n="${GIT_CONFIG_COUNT:-0}"
  case "$n" in "" | *[!0-9]*) n=0 ;; esac
  export "GIT_CONFIG_KEY_$n=$1" "GIT_CONFIG_VALUE_$n=$2"
  export GIT_CONFIG_COUNT=$((n + 1))
}

apply_identity() {
  if [[ -n "$cred_name" ]]; then
    export GIT_AUTHOR_NAME="$cred_name" GIT_COMMITTER_NAME="$cred_name"
    add_git_config user.name "$cred_name"
  fi
  if [[ -n "$cred_email" ]]; then
    export GIT_AUTHOR_EMAIL="$cred_email" GIT_COMMITTER_EMAIL="$cred_email"
    add_git_config user.email "$cred_email"
  fi
}

# Starts the token server and points Git at it. Without a token, nothing.
serve_token() {
  [[ -n "$cred_token" ]] || return 0
  local base sock helper _
  base="${BOATPARTY_RUNNER_SHM:-/dev/shm}"
  [[ -d "$base" && -w "$base" ]] || base="${TMPDIR:-/tmp}"
  # A Unix socket path is limited to about 100 bytes.
  if ((${#base} > 70)); then base=/tmp; fi
  if ! cred_dir="$(mktemp -d "$base/bpc-XXXXXXXX" 2>/dev/null)"; then
    cred_dir=""
    cred_token=""
    echo "boatparty: could not set up the GitHub token for the command; it runs without one" >&2
    return 0
  fi
  chmod 700 "$cred_dir"
  sock="$cred_dir/s"
  # The token reaches perl through a pipe from a builtin, never an argument.
  perl -MSocket -e '
    my ($path) = @ARGV;
    my $parent = getppid();
    my $token = <STDIN>;
    close STDIN;
    exit 1 unless defined $token;
    chomp $token;
    exit 1 unless length $token;
    socket(my $server, PF_UNIX, SOCK_STREAM, 0) or exit 1;
    umask 077;
    bind($server, pack_sockaddr_un("$path.new")) or exit 1;
    listen($server, 64) or exit 1;
    rename("$path.new", $path) or exit 1;
    $SIG{PIPE} = "IGNORE";
    $SIG{TERM} = $SIG{INT} = $SIG{HUP} = sub { unlink $path; exit 0 };
    while (1) {
      if (getppid() != $parent) { unlink $path; exit 0 }
      my $ready = "";
      vec($ready, fileno($server), 1) = 1;
      next unless select($ready, undef, undef, 1) > 0;
      accept(my $client, $server) or next;
      syswrite($client, "$token\n");
      close $client;
    }' "$sock" < <(printf '%s\n' "$cred_token") >/dev/null 2>&1 9>&- &
  cred_pid=$!
  cred_token=""
  for _ in $(seq 1 100); do
    [[ -S "$sock" ]] && break
    kill -0 "$cred_pid" 2>/dev/null || break
    sleep 0.05
  done
  if [[ ! -S "$sock" ]]; then
    stop_token
    echo "boatparty: the GitHub token server did not start; the command runs without a token" >&2
    return 0
  fi
  export BOATPARTY_CREDENTIAL_SOCKET="$sock"
  # Git runs "<helper> get|store|erase" through the shell.
  helper="!bash $(printf %q "$0") credential $(printf %q "$project") $(printf %q "$cred_url")"
  add_git_config "credential.$cred_url.helper" ""
  add_git_config "credential.$cred_url.helper" "$helper"
  if [[ "$cred_url" == https://github.com ]]; then
    # Dependencies named by SSH URL use the token too (the sandbox has no
    # GitHub SSH key).
    add_git_config url.https://github.com/.insteadOf git@github.com:
    add_git_config url.https://github.com/.insteadOf ssh://git@github.com/
  fi
}

stop_token() {
  cred_token=""
  if [[ -n "$cred_pid" ]]; then
    kill "$cred_pid" 2>/dev/null || true
    wait "$cred_pid" 2>/dev/null || true
    cred_pid=""
  fi
  if [[ -n "$cred_dir" ]]; then
    rm -rf "$cred_dir"
    cred_dir=""
  fi
  unset BOATPARTY_CREDENTIAL_SOCKET
}

# credential <project> <url> <action>: Git's credential helper inside a
# command run with a token. Answers "get" for <url>'s protocol and host only,
# with the token from the command's socket; ignores "store" and "erase".
mode_credential() {
  local url="${1:-}" action="${2:-}" line protocol="" host=""
  while IFS= read -r line; do
    [[ -z "$line" ]] && break
    case "$line" in
      protocol=*) protocol="${line#protocol=}" ;;
      host=*) host="${line#host=}" ;;
    esac
  done
  [[ "$action" == get && -n "$url" && "$protocol://$host" == "$url" ]] || return 0
  [[ -S "${BOATPARTY_CREDENTIAL_SOCKET:-}" ]] || return 0
  perl -MSocket -e '
    socket(my $s, PF_UNIX, SOCK_STREAM, 0) or exit 0;
    connect($s, pack_sockaddr_un($ARGV[0])) or exit 0;
    my $token = <$s>;
    exit 0 unless defined $token;
    chomp $token;
    exit 0 unless length $token;
    print "username=x-access-token\npassword=$token\n";' "$BOATPARTY_CREDENTIAL_SOCKET"
}

