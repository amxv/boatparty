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
    if port_listening "$port"; then
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
    if [[ -f "$file" ]] && ! kill -0 "$(cat "$file" 2>/dev/null)" 2>/dev/null; then
      rm -f "$file"
    fi
  done
}
