#!/usr/bin/env bash
# Stage 12.9 item 4 lane (P12-U003): vt parity between the screen the host
# delivered and the screen tmux itself has.
#
# The fixture pairs a flood-able plain shell pane with an agent-TUI pane
# (alternate screen, cursor addressing, DEC 2026 brackets, bold/dim runs, 1 Hz
# repaint). At five quiesce points — seed, hide/reveal, pause/continue, an
# explicit seed request, and a full bridge reconnect — the driver replays every
# byte the host delivered for each pane through a vt parser sized to the pane's
# tmux grid and diffs the resulting screen against `capture-pane -p -e` rendered
# through a second parser of the same size. Text and attribute parity are both gates.
#
# Isolated tmux socket + isolated runtime dir under /tmp (macOS caps UDS paths
# at 103 bytes); the user's tmux is never touched.
set -euo pipefail
trap 'echo "vt-parity lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
runtime="/tmp/adevtp-$run_id"
socket="ade-vt-parity-$$"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/tmux-ide-host"
driver_binary="$driver_target/release/performance-test-driver"
stall_seconds="${1:-8}"
# Under the 1 Hz repaint, "no new bytes for this long" is the pane standing
# still between frames.
quiesce_ms="${2:-700}"

daemon_pid=""
cleanup() {
  [[ -n "$daemon_pid" ]] && kill -CONT "$daemon_pid" 2>/dev/null || true
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

# An explicit grid: the parity comparison is only as meaningful as the geometry
# both sides agree on, and a detached session's default size is not stated
# anywhere the lane can read it back from.
tmux -L "$socket" new-session -d -x 120 -y 36 -s vtparity 'exec bash --norc'
# The hold file is how the driver stops the fixture repainting while it checks a
# recovery; the path is the driver's own convention ($runtime/agent-hold).
tmux -L "$socket" split-window -t vtparity \
  "exec bash $repo_root/tests/performance/runtime/agent-fixture.sh $runtime/agent-hold"

ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

ADE_PHASE1_TESTING=1 "$driver_binary" vt-parity \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$runtime" \
  --tmux-socket "$socket" \
  --primary vtparity \
  --label "vt-parity-local-${stall_seconds}s" \
  --stall-seconds "$stall_seconds" \
  --quiesce-ms "$quiesce_ms" \
  --stall-script "kill -STOP $daemon_pid" \
  --restore-script "kill -CONT $daemon_pid" \
  >"$runtime/vt-parity.json"

cat "$runtime/vt-parity.json"

jq -r '
  "attribute mismatches: " + (
    [.quiescePoints | to_entries[] | "\(.key)=\(.value.attributeMismatchCells)"] | join(" ")
  )
' "$runtime/vt-parity.json"

# Attributes are gated, not just reported: mid-word bold/dim tearing is one of
# the three artifacts item 4 exists to close, and a lane that prints an
# attribute regression without failing on it would let that one back in.
jq -e '
  (.quiescePoints | has("seed") and has("hideReveal") and has("pauseContinue")
    and has("recovery") and has("reconnect"))
  and (.quiescePoints | to_entries | all(.value.textMismatchRows == 0))
  and (.quiescePoints | to_entries | all(.value.attributeMismatchCells == 0))
  and (.connectionWideResyncEvents == 0)
  and (.sequenceGapObserved | not)
' "$runtime/vt-parity.json" >/dev/null || {
  echo "FAIL  vt-parity  a delivered screen did not match tmux ground truth" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}
printf 'PASS  vt-parity  delivered screens matched tmux at all five quiesce points\n'
printf 'artifacts: %s\n' "$runtime"
