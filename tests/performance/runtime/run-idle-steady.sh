#!/usr/bin/env bash
# Stage 12.9 steadiness lane: does an attached session do work when nothing is
# happening?
#
# A pane that is reseeded repeatedly repaints its whole screen repeatedly, which
# is what continuous flickering looks like from outside, and repeated topology
# pushes are what "topology changed" notices are. Latency numbers cannot see
# either, so this counts events over two windows: one with the agent fixture
# held completely still, one with it repainting once a second.
#
# Isolated tmux socket + isolated runtime dir under /tmp (macOS caps UDS paths
# at 103 bytes); the user's tmux is never touched.
set -euo pipefail
trap 'echo "idle-steady lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
runtime="/tmp/adeidle-$run_id"
socket="ade-idle-steady-$$"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/muxflow-host"
driver_binary="$driver_target/release/performance-test-driver"
window_seconds="${1:-60}"

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
cargo build --release --bin muxflow-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1

tmux -L "$socket" new-session -d -x 120 -y 36 -s idlesteady 'exec bash --norc'
tmux -L "$socket" split-window -t idlesteady \
  "exec bash $repo_root/tests/performance/runtime/agent-fixture.sh $runtime/agent-hold"

ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

ADE_PHASE1_TESTING=1 "$driver_binary" idle-steady \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$runtime" \
  --tmux-socket "$socket" \
  --primary idlesteady \
  --hold-file "$runtime/agent-hold" \
  --window-seconds "$window_seconds" \
  --label "idle-steady-local-${window_seconds}s" \
  >"$runtime/idle-steady.json"

cat "$runtime/idle-steady.json"

jq -e '
  # Nothing at all while the fixture is still.
  .idle.terminalSeeds == 0 and .idle.topologyDirty == 0
  and .idle.resnapshotsRequired == 0 and .idle.connectionWideResyncs == 0
  and .idle.agentBytesDelivered == 0
  # And a pane repainting once a second is delivered as output, never as a
  # rebuilt screen: a reseed here is a repaint the user sees as a flash.
  and .repainting.terminalSeeds == 0 and .repainting.topologyDirty == 0
  and .repainting.resnapshotsRequired == 0 and .repainting.connectionWideResyncs == 0
  and .repainting.agentBytesDelivered > 0
  and (.sequenceGapObserved | not)
' "$runtime/idle-steady.json" >/dev/null || {
  echo "FAIL  idle-steady  the session did work nobody asked for" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}
printf 'PASS  idle-steady  no reseed, no topology churn, no resync in %ss idle + %ss repainting\n' \
  "$window_seconds" "$window_seconds"
printf 'artifacts: %s\n' "$runtime"
