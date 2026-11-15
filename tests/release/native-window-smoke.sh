#!/usr/bin/env bash
set -euo pipefail

# Exercise the installed package's real X11 swapchain. Offscreen frames and a
# mapped window alone cannot catch a native window which presents black pixels.
# Requires Xvfb, xauth, x11-utils, ImageMagick, dbus-run-session and tmux.
archive=$(realpath "${1:?usage: native-window-smoke.sh PACKAGE.tar.gz}")
for command in xvfb-run xwininfo xprop import identify convert compare dbus-run-session tmux timeout; do
  command -v "$command" >/dev/null || { echo "missing native smoke dependency: $command" >&2; exit 69; }
done
stage=$(mktemp -d /tmp/muxflow-native.XXXXXX)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/package" "$stage/home" "$stage/config" "$stage/data" "$stage/runtime" "$stage/host"
chmod 700 "$stage/runtime" "$stage/host"
tar -xzf "$archive" -C "$stage/package"
package=$(find "$stage/package" -mindepth 1 -maxdepth 1 -type d -print -quit)
[[ -f "$package/install.sh" ]] || { echo "package installer missing" >&2; exit 1; }
export ADE_INSTALL_PREFIX="$stage/prefix"
export ADE_HOST_RUNTIME_DIR="$stage/host" ADE_TMUX_SOCKET_NAME="muxflow-native-$$"
export HOME="$stage/home" XDG_CONFIG_HOME="$stage/config" XDG_DATA_HOME="$stage/data" XDG_RUNTIME_DIR="$stage/runtime"
export MUXFLOW_NATIVE_EVIDENCE="${MUXFLOW_NATIVE_SMOKE_ARTIFACT_DIR:-$stage/evidence}"
mkdir -p "$MUXFLOW_NATIVE_EVIDENCE"
bash "$package/install.sh" >"$MUXFLOW_NATIVE_EVIDENCE/install.log"
export ADE_HOST_HELPER_PATH="$ADE_INSTALL_PREFIX/lib/muxflow/muxflow-host"
export PATH="$ADE_INSTALL_PREFIX/bin:$PATH"
unset TMUX WAYLAND_DISPLAY
xvfb-run -a -s '-screen 0 1600x1000x24 -nolisten tcp' dbus-run-session -- bash <<'RUN'
set -euo pipefail
app_pid=
wm_pid=
cleanup() {
  [[ -z "$app_pid" ]] || kill "$app_pid" 2>/dev/null || true
  [[ -z "$wm_pid" ]] || kill "$wm_pid" 2>/dev/null || true
  muxflow-host daemon-stop --force >/dev/null 2>&1 || true
  tmux -L "$ADE_TMUX_SOCKET_NAME" kill-server >/dev/null 2>&1 || true
}
trap cleanup EXIT
if [[ ${MUXFLOW_NATIVE_SMOKE_COMPOSITOR:-0} == 1 ]]; then
  command -v xfwm4 >/dev/null
  xfwm4 --compositor=on >"$MUXFLOW_NATIVE_EVIDENCE/compositor.log" 2>&1 &
  wm_pid=$!
  sleep 2
  kill -0 "$wm_pid"
fi
# Mutate the isolated tmux server through its CLI, never through GUI input. The
# host's workspace subscription must cause the installed app to redraw later.
tmux -L "$ADE_TMUX_SOCKET_NAME" -f /dev/null new-session -d -s native-before 'sleep 300'
muxflow >"$MUXFLOW_NATIVE_EVIDENCE/app.log" 2>&1 &
app_pid=$!
window=
# X11 publishes the window and PID before mapping finishes. A failed capture
# during startup is not yet a rendering failure; the bounded loop below still
# requires a viewable window with nonuniform pixels.
capture_window() {
  xwininfo -id "$window" | grep -q 'Map State: IsViewable' &&
    timeout 5 import -window "$window" "$1"
}
for attempt in $(seq 1 30); do
  kill -0 "$app_pid" || { cat "$MUXFLOW_NATIVE_EVIDENCE/app.log"; exit 1; }
  window=$(xwininfo -root -tree | awk '/"Muxflow"/ {print $1; exit}')
  if [[ -n "$window" ]] && xprop -id "$window" _NET_WM_PID | grep -q "= $app_pid$"; then
    if capture_window "$MUXFLOW_NATIVE_EVIDENCE/first.png"; then
      colors=$(identify -format '%k' "$MUXFLOW_NATIVE_EVIDENCE/first.png")
      if (( colors >= 32 )); then break; fi
    fi
  fi
  sleep 1
done
if [[ -z "$window" || ${colors:-0} -lt 32 ]]; then
  echo "installed app failed to paint nonuniform native-window content within 30 seconds" >&2
  cat "$MUXFLOW_NATIVE_EVIDENCE/app.log"
  exit 1
fi
# Wait for the initial workspace snapshot, then compare only the sidebar's
# workspace region: clocks and connection latency elsewhere cannot pass this.
sleep 3
capture_window "$MUXFLOW_NATIVE_EVIDENCE/before.png"
convert "$MUXFLOW_NATIVE_EVIDENCE/before.png" -crop 300x250+0+40 +repage "$MUXFLOW_NATIVE_EVIDENCE/before-sidebar.png"
tmux -L "$ADE_TMUX_SOCKET_NAME" rename-session -t native-before REDRAW-confirmed
for attempt in $(seq 1 15); do
  sleep 1
  if ! capture_window "$MUXFLOW_NATIVE_EVIDENCE/after.png"; then continue; fi
  convert "$MUXFLOW_NATIVE_EVIDENCE/after.png" -crop 300x250+0+40 +repage "$MUXFLOW_NATIVE_EVIDENCE/after-sidebar.png"
  if ! compare -metric AE "$MUXFLOW_NATIVE_EVIDENCE/before-sidebar.png" "$MUXFLOW_NATIVE_EVIDENCE/after-sidebar.png" null: 2>"$MUXFLOW_NATIVE_EVIDENCE/changed-pixels.txt"; then
    changed=$(cat "$MUXFLOW_NATIVE_EVIDENCE/changed-pixels.txt")
    if [[ "$changed" =~ ^[0-9]+$ ]] && (( changed >= 20 )); then
      echo "Installed native window painted ($colors colors) and redrew after workspace rename ($changed changed sidebar pixels)."
      exit 0
    fi
  fi
done
echo "installed app did not repaint its workspace region after a host-side rename" >&2
exit 1
RUN
