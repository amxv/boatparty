# Boat's streaming services are image details. Restrict service management
# to the sandbox state directory. Local runner hosts supply a controller
# shim explicitly to exercise this code without changing the test machine.
desktop_control() {
  if [[ -n "${BOATPARTY_RUNNER_SYSTEMCTL:-}" ]]; then
    "$BOATPARTY_RUNNER_SYSTEMCTL" "$@"
  elif [[ "$state" == /home/user/.boatparty ]]; then
    sudo -n systemctl "$@"
  else
    return 1
  fi
}

desktop_units() {
  local unit
  for unit in lightdm sunshine moonlight-web ascii-streaming-audio gnome-remote-desktop; do
    if desktop_control cat "$unit.service" >/dev/null 2>&1; then printf '%s\n' "$unit.service"; fi
  done
}

# Serialize service transitions separately from checkout work. An explicit
# request pins the stack until the next boot. A GUI server holds it only
# while its runner is alive, including the interval before the server starts.
desktop_start() (
  local units=() unit
  exec 6>"$state/desktop.lock"
  if command -v flock >/dev/null 2>&1; then flock 6; fi
  while IFS= read -r unit; do units+=("$unit"); done < <(desktop_units)
  (("${#units[@]}")) || { echo "boatparty: desktop services are unavailable" >&2; return 1; }
  desktop_control start "${units[@]}" >&2 || return 1
  if [[ "$1" == request ]]; then
    boot_id >"$state/desktop-requested"
  else
    echo "$$" >"$state/out/$1.desktop"
  fi
)

desktop_idle() {
  [[ "$bp_desktop_keep" == 0 ]] || return 0
  [[ "$state" == /home/user/.boatparty || -n "${BOATPARTY_RUNNER_SYSTEMCTL:-}" ]] || return 0
  (
    exec 6>"$state/desktop.lock"
    if command -v flock >/dev/null 2>&1; then flock 6; fi
    [[ -f "$state/desktop-requested" && "$(cat "$state/desktop-requested")" == "$(boot_id)" ]] && exit 0
    local marker unit units=()
    for marker in "$state"/out/*.desktop; do
      [[ -f "$marker" ]] || continue
      kill -0 "$(cat "$marker")" 2>/dev/null && exit 0
      rm -f "$marker"
    done
    while IFS= read -r unit; do
      if desktop_control is-active --quiet "$unit"; then units+=("$unit"); fi
    done < <(desktop_units)
    if (("${#units[@]}")); then desktop_control stop "${units[@]}"; fi
  ) </dev/null >/dev/null 2>&1 5>&- &
}

desktop_env() {
  local pid kv attempt
  for ((attempt=0; attempt<60; attempt++)); do
    pid="$(pgrep -u "$(id -u)" -n -x 'openbox|xfce4-session|gnome-session.*|startplasma.*' || true)"
    if [[ -n "$pid" && -r "/proc/$pid/environ" ]]; then
      while IFS= read -r -d '' kv; do
        case "$kv" in
          DISPLAY=*|XAUTHORITY=*|DBUS_SESSION_BUS_ADDRESS=*|XDG_RUNTIME_DIR=*|XDG_SESSION_TYPE=*|XDG_CURRENT_DESKTOP=*) export "${kv?}" ;;
        esac
      done <"/proc/$pid/environ"
      [[ -n "${DISPLAY:-}" ]] && return 0
    fi
    sleep 1
  done
  echo "boatparty: timed out waiting for a desktop session" >&2
  return 1
}

window_ready() {
  local sid="$1" window owner
  command -v xdotool >/dev/null || return 1
  for window in $(xdotool search --onlyvisible --name '' 2>/dev/null); do
    owner="$(xdotool getwindowpid "$window" 2>/dev/null)" || continue
    [[ "$(ps -p "$owner" -o sid= | tr -d ' ')" == "$sid" ]] && return 0
  done
  return 1
}
