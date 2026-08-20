#!/usr/bin/env bash
# Stage 12.9 lane: agent-pane realtime correctness under tmux pause-after flow
# control (P12-U001 repro). Freezes the host daemon with SIGSTOP while a pane
# floods so tmux's pause-after=5 engages, then requires both a visible and a
# hidden pane to resume and match capture-pane ground truth.
#
# Isolated tmux socket + isolated runtime dir; the user's tmux is never touched.
# The runtime dir lives in /tmp because macOS caps UDS paths at 103 bytes.
#
# WEDGE_PROBE=1 additionally samples the daemon and tmux client mid-run, for
# post-mortem when the probe fails.
#
# REJECT_RESUME=<n> runs the same lane with the daemon's first n
# `refresh-client -A` writes deliberately unquoted, so tmux's lexer refuses
# them (`ADE_TEST_REJECT_FLOW_RESUME`, the P12-U001 shape). That is the half a
# healthy tmux never exercises: the recovery *from* a rejected resume, which
# before this had none — the resume was issued once, its rejection was reported,
# and the reseed that report asked for re-photographed a pane that was still
# paused. The bar is the same as the healthy run, both panes advancing and
# matching capture-pane ground truth, plus proof the rejection actually
# happened and that no pane ended up stalled.
set -euo pipefail
trap 'echo "pause-probe failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
runtime="/tmp/adepause-$run_id"
socket="ade-pause-probe-$$"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/muxflow-host"
driver_binary="$driver_target/release/performance-test-driver"
stall_seconds="${1:-8}"

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
cargo build --release --bin muxflow-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1

# Fixture: pane A ticks once per second forever, pane B is an idle shell the
# probe floods. The TICK counter is the machine-checkable progress signal.
tmux -L "$socket" new-session -d -s pauseprobe \
  'i=0; while true; do printf "\rTICK %d  " "$i"; i=$((i+1)); sleep 1; done'
tmux -L "$socket" split-window -t pauseprobe 'exec bash'

# Deliberately unquoted below: it is either empty or one fixed assignment, and
# bash 3.2 — which is what macOS ships — expands an empty array to an empty
# word, which would become the command name.
reject_resume_env=""
if [[ -n "${REJECT_RESUME:-}" ]]; then
  reject_resume_env="ADE_TEST_REJECT_FLOW_RESUME=$REJECT_RESUME"
fi

env ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  $reject_resume_env \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

ADE_PHASE1_TESTING=1 "$driver_binary" pause-probe \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$runtime" \
  --tmux-socket "$socket" \
  --primary pauseprobe \
  --label "pause-probe-local-${stall_seconds}s" \
  --stall-seconds "$stall_seconds" \
  --stall-script "kill -STOP $daemon_pid" \
  --restore-script "kill -CONT $daemon_pid" \
  >"$runtime/pause-probe.json" &
driver_pid=$!

if [[ "${WEDGE_PROBE:-}" == "1" ]]; then
  sleep 42
  {
    echo "== process tree =="
    ps -o pid,stat,command -p "$daemon_pid" || true
    for child in $(pgrep -P "$daemon_pid" || true); do
      ps -o pid,stat,command -p "$child" || true
    done
    echo "== tmux clients =="
    tmux -L "$socket" list-clients \
      -F '#{client_name} flags=#{client_flags} written=#{client_written}' || true
  } >"$runtime/wedge-state.txt" 2>&1
  sample "$daemon_pid" 2 -file "$runtime/wedge-daemon-sample.txt" >/dev/null 2>&1 || true
fi

wait "$driver_pid"
cat "$runtime/pause-probe.json"

jq -e '
  .baselineTicking
  and .round1.floodResumed and .round1.tickerAdvanced and .round1.matchesGroundTruth
  and .round2Hidden.floodResumed and (.round2Hidden.revealError == null)
  and .round2Hidden.tickerAdvanced and .round2Hidden.matchesGroundTruth
  and (.connectionWideResyncEvents == 0)
  and (.flowPausedEvents >= 1)
' "$runtime/pause-probe.json" >/dev/null || {
  echo "FAIL  pause-probe  a pane did not survive tmux pause-after flow control" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}

# No pane may end a run stalled. On a healthy tmux nothing should reject a
# resume at all; under REJECT_RESUME the retry is what has to have covered it,
# and a stall means it did not.
jq -e '.flowStalledEvents == 0' "$runtime/pause-probe.json" >/dev/null || {
  echo "FAIL  pause-probe  a pane was left stalled by tmux flow control" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}

# Read from the daemon log rather than the driver's events: a rejection the
# host retries and recovers from is deliberately *not* an event — the desktop
# has nothing to do about it — so the log is the only place it exists.
rejections="$(grep -c '"event":"flowResumeRejected"' "$runtime/daemon.log" || true)"
if [[ -n "${REJECT_RESUME:-}" ]]; then
  # Otherwise the run proved only what the healthy run already proves.
  [[ "$rejections" -ge 1 ]] || {
    echo "FAIL  pause-probe  REJECT_RESUME=$REJECT_RESUME rejected nothing; the fault never fired" >&2
    echo "artifacts: $runtime" >&2
    exit 1
  }
  printf 'PASS  pause-probe  panes recovered from %s rejected resumes after %ss stall\n' \
    "$rejections" "$stall_seconds"
  printf 'artifacts: %s\n' "$runtime"
  exit 0
fi

[[ "$rejections" -eq 0 ]] || {
  echo "FAIL  pause-probe  a resume was rejected on a healthy tmux" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}
printf 'PASS  pause-probe  panes resumed and matched ground truth after %ss stall\n' "$stall_seconds"
printf 'artifacts: %s\n' "$runtime"
