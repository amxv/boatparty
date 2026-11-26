# Servers (`serve` mode, `boatparty serve`).
#
# A server runs exactly like a `run` command, plus:
# - out/<nonce>.serve holds its pid while it runs, so commands beside it skip
#   flooring build outputs (serve_active) while it keeps building;
# - with a ready port, the event "ready" is sent once when something listens
#   on that port on the sandbox.
# It stops the same way a command does: end of file on stdin (the client
# cancelled, or died and its connection closed) terminates its process group.

serve_mode=0

# Whether something listens on TCP port $1 on this machine. Reads the
# kernel's socket tables, so nothing connects to the server; without them a
# connect probe on the loopback address stands in.
port_listening() {
  local port="$1" hex
  hex="$(printf '%04X' "$port")"
  if [[ -r /proc/net/tcp ]]; then
    # "sl local_address rem_address st": state 0A is LISTEN.
    grep -qE "^ *[0-9]+: [0-9A-F]+:$hex [0-9A-F]+:[0-9A-F]+ 0A " /proc/net/tcp /proc/net/tcp6 2>/dev/null
  else
    (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null
  fi
}

# watch_ready <nonce> <port> <pid>: send "ready" once the port listens, for
# as long as the server's process group lives. Runs in the background with
# the runner's stderr, which carries the event.
watch_ready() {
  local nonce="$1" port="$2" pid="$3"
  while kill -0 -- "-$pid" 2>/dev/null || kill -0 "$pid" 2>/dev/null; do
    if { [[ "$port" == window ]] && window_ready "$pid"; } ||
       { [[ "$port" != window ]] && port_listening "$port"; }; then
      printf '\n%s!ready\n' "$nonce" >&2
      return 0
    fi
    sleep 0.5
  done
}

# Drop serve markers whose server is gone (a runner that was killed itself
# leaves its marker behind).
prune_serve_markers() {
  local file
  for file in "$state"/out/*.serve; do
    if [[ -f "$file" ]] && ! kill -0 "$(cat "$file" 2>/dev/null)" 2>/dev/null &&
       ! pgrep -s "$(cat "$file" 2>/dev/null)" >/dev/null 2>&1; then
      rm -f "$file"
    fi
  done
}

# Identify only our own GUI/dev server sessions, including a port holder
# whose marker disappeared. A session inherited the runner's nonce at
# exec. Its live runner remains its parent while the SSH client is attached.
reap_orphan() {
  local pid="$1" expected="${2:-}" sid token parent
  [[ "$pid" =~ ^[0-9]+$ && -r "/proc/$pid/environ" ]] || return 0
  sid="$(ps -p "$pid" -o sid= 2>/dev/null | tr -d ' ')" || return 0
  [[ "$sid" =~ ^[0-9]+$ && "$sid" -gt 1 ]] || return 0
  token="$(tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | sed -n 's/^BOATPARTY_SERVER_SESSION=//p')" || return 0
  [[ "$token" =~ ^[a-zA-Z0-9_-]+$ && ( -z "$expected" || "$token" == "$expected" ) ]] || return 0
  # A live runner is the session leader's parent. A dead runner leaves it
  # reparented, or leaves only children of the vanished session leader.
  parent="$(ps -p "$sid" -o ppid= 2>/dev/null | tr -d ' ')" || parent=""
  [[ -n "$parent" && "$parent" == "$(cat "$state/out/$token.owner" 2>/dev/null)" ]] && return 0
  [[ "$sid" != "$(ps -p $$ -o sid= | tr -d ' ')" ]] || return 0
  pkill -TERM -s "$sid" 2>/dev/null || return 0
  local attempt
  for ((attempt=0; attempt<10; attempt++)); do
    pgrep -s "$sid" >/dev/null 2>&1 || break
    sleep 0.5
  done
  pkill -KILL -s "$sid" 2>/dev/null || true
  echo "boatparty: stopped orphaned server session $sid" >&2
}

reap_servers() {
  local file token holder port="$1"
  for file in "$state"/out/*.serve; do
    [[ -f "$file" ]] || continue
    token="${file##*/}"
    local sid member
    sid="$(cat "$file" 2>/dev/null)" || continue
    [[ "$sid" =~ ^[0-9]+$ ]] || continue
    member="$sid"
    if ! kill -0 "$sid" 2>/dev/null; then
      member="$(pgrep -s "$sid" 2>/dev/null | head -1)" || continue
    fi
    reap_orphan "$member" "${token%.serve}"
  done
  if [[ "$port" =~ ^[0-9]+$ ]]; then
    while IFS= read -r holder; do reap_orphan "$holder"; done < <(
      ss -ltnpH "sport = :$port" 2>/dev/null | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u
    )
  fi
}
