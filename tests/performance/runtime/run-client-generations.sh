#!/usr/bin/env bash
# M13-E005: does every control client that becomes the visible one get the
# app's size, or only the first?
#
# `run-app-client-size.sh` launches the packaged app and gates that the size it
# asks for never *moves*. It cannot see this defect, because everything it does
# happens inside one tmux session: the client the app sizes on connect is the
# only client it ever observes. The user hit the second one — a workspace on
# another session, selected after connect — whose control client was taken out
# of `ignore-size` without ever being told a size, so tmux sized the windows
# they were looking at from its own 80x24 default and the agent inside them
# painted into a quarter of the surface.
#
# The switch is driven through the host protocol rather than the app's UI
# because the size is deliberately *not* re-sent by the desktop when the user
# changes workspace: the app's surface has not moved, so it has nothing new to
# say. That is what makes this the host's invariant, and it is the half a
# GUI-driven lane cannot isolate.
#
# Safety: everything happens on a throwaway tmux server on a private socket,
# under a private runtime directory, with a private daemon. The user's tmux
# servers and daemon are never contacted.
#
#   bash tests/performance/runtime/run-client-generations.sh
set -euo pipefail
trap 'echo "client-generations lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
session="ade-phase12-13-gen-$run_id"
runtime="/tmp/adegen-$run_id"
socket="ade-gen-$run_id"
columns="${ADE_GEN_COLUMNS:-180}"
rows="${ADE_GEN_ROWS:-45}"

main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/tmux-ide-host"
driver_binary="$driver_target/release/performance-test-driver"

daemon_pid=""
cleanup() {
  ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null || true
  tmux -L "$socket" kill-server >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$runtime"
chmod 0700 "$runtime"

cd "$repo_root"
cargo build --release --bin tmux-ide-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1

# tmux's own default, from a server no client has sized: the size the second
# session's windows come up at, and the size they stay at when the defect is
# present.
env -u TMUX tmux -L "$socket" -f /dev/null \
  new-session -d -x 80 -y 24 -s "$session" 'exec bash --norc'
default_size="$(env -u TMUX tmux -L "$socket" list-windows -a -F '#{window_width}x#{window_height}' | sort -u | tr '\n' ' ')"

ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

report="$runtime/client-generations.json"
if ADE_PHASE1_TESTING=1 "$driver_binary" client-generations \
  --transport local --host-binary "$host_binary" --runtime "$runtime" \
  --tmux-socket "$socket" --primary "$session" \
  --columns "$columns" --rows "$rows" \
  --label "client-generations-local" >"$report" 2>"$runtime/driver.log"; then
  cat "$report"
  windows="$(env -u TMUX tmux -L "$socket" list-windows -a -F '#{window_width}x#{window_height}' | sort -u | tr '\n' ' ')"
  printf 'PASS  client-generations  every visible client reached %sx%s; tmux default before the app was %s(now %s)\n' \
    "$columns" "$rows" "$default_size" "${windows% }"
  printf 'artifacts: %s\n' "$runtime"
else
  cat "$report" 2>/dev/null || true
  cat "$runtime/driver.log" >&2
  echo "FAIL  client-generations  a control client became visible without being given the app's size" >&2
  echo "artifacts: $runtime" >&2
  exit 1
fi
