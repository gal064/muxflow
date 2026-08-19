#!/usr/bin/env bash
# Stage 12.9 item 3 lane (P12-U002): keystroke wire echo in a plain shell pane
# next to an agent-shaped pane, on the same tmux server, over the same host.
#
# The question this answers is "is the typing lag in agent panes on the wire, or
# above it". Both panes are measured with the same probe; only the shape of what
# they emit differs. The renderer is not in this path — the renderer-side figure
# is the agent-repaint frame cost reported by the desktop test suite.
#
# Isolated tmux socket + isolated runtime dir under /tmp (macOS caps UDS paths
# at 103 bytes); the user's tmux is never touched.
set -euo pipefail
trap 'echo "agent-echo lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
runtime="/tmp/adeagent-$run_id"
socket="ade-agent-echo-$$"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/tmux-ide-host"
driver_binary="$driver_target/release/performance-test-driver"
samples="${1:-35}"
# The local keystroke budget from the plan's acceptance table.
budget_ms=35

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

tmux -L "$socket" new-session -d -s agentecho 'exec bash'
tmux -L "$socket" split-window -t agentecho \
  "exec bash $repo_root/tests/performance/runtime/agent-fixture.sh"

ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

ADE_PHASE1_TESTING=1 "$driver_binary" agent-echo \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$runtime" \
  --tmux-socket "$socket" \
  --primary agentecho \
  --samples "$samples" \
  --label "agent-echo-local" \
  >"$runtime/agent-echo.json"

cat "$runtime/agent-echo.json"

# The ratio is INFO, not a gate: the plain lane answers a keystroke with one
# echoed byte and the agent lane with a 4 KiB repaint, so it measures payload
# size more than it measures any agent-specific cost. See plan §12.9 item 3c,
# amended with this measurement in hand.
jq -r --argjson budget "$budget_ms" '
  "plain p95 \(.plainEchoMs.p95Ms) ms | agent p95 \(.agentEchoMs.p95Ms) ms | " +
  "ratio (INFO) \((.agentEchoMs.p95Ms / .plainEchoMs.p95Ms) * 1000 | round / 1000) | " +
  "agent bytes \(.agentBytesDelivered) | budget \($budget) ms"
' "$runtime/agent-echo.json"

jq -e --argjson budget "$budget_ms" '
  .plainEchoMs.p95Ms <= $budget
  and .agentEchoMs.p95Ms <= $budget
  and .connectionWideResyncEvents == 0
  and (.sequenceGapObserved | not)
' "$runtime/agent-echo.json" >/dev/null || {
  echo "FAIL  agent-echo  a lane exceeded the ${budget_ms} ms keystroke budget" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}
printf 'PASS  agent-echo  both lanes within the %s ms keystroke budget\n' "$budget_ms"
printf 'artifacts: %s\n' "$runtime"
