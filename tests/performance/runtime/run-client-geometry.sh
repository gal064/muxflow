#!/usr/bin/env bash
# Stage 12.10 item 4: can a connected app leave a tmux window bigger than the
# terminal a human is looking at, and does an idle connection stay idle?
#
# P12-U006 resized the user's real windows to ~300 rows because the desktop
# derived the tmux client size from a pane's share of the topology. The desktop
# now derives it from its own surface, so it asks for one fixed size no matter
# what the panes do; this lane sends exactly that — before and after every
# split, zoom, window switch and pane resize — against a scratch session that a
# second, fixed-size control client is also attached to, and checks that no
# window on the server ever exceeds that client's size.
#
# Safety: everything happens inside a session named `ade-phase12-geo-*`, created
# and killed here. Remote runs touch the user's tmux server only through that
# session. Nothing outside it is created, resized, killed or written to.
#
#   bash tests/performance/runtime/run-client-geometry.sh                       # local, short
#   ADE_GEO_TARGET=remote-linux ADE_GEO_IDLE_SECONDS=600 \
#     bash tests/performance/runtime/run-client-geometry.sh                     # against remote-linux
set -euo pipefail
trap 'echo "client-geometry lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
session="ade-phase12-geo-$run_id"
runtime="/tmp/adegeo-$run_id"
socket="ade-geo-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/muxflow-host"
driver_binary="$driver_target/release/performance-test-driver"

target="${ADE_GEO_TARGET:-}"                 # empty = local isolated tmux server
ssh_config="${ADE_GEO_SSH_CONFIG:-$HOME/.ssh/config}"
remote_runtime="${ADE_GEO_REMOTE_RUNTIME:-/tmp/muxflow-1000}"
columns="${ADE_GEO_COLUMNS:-180}"
rows="${ADE_GEO_ROWS:-45}"
bound_columns="${ADE_GEO_BOUND_COLUMNS:-188}"
bound_rows="${ADE_GEO_BOUND_ROWS:-51}"
churn_rounds="${ADE_GEO_CHURN_ROUNDS:-6}"
idle_seconds="${ADE_GEO_IDLE_SECONDS:-120}"

mkdir -p "$runtime"
chmod 0700 "$runtime"

# One place that decides where tmux runs. Local runs use a throwaway server so
# the developer's own tmux is never a candidate; remote runs use the user's
# server, which is exactly what makes the scratch-session discipline above the
# only thing standing between this lane and their work.
if [[ -n "$target" ]]; then
  remote() { ssh -F "$ssh_config" -o BatchMode=yes -o ConnectTimeout=15 "$target" "$@"; }
  # The transport flattens a command into one remote shell line, so each
  # argument is quoted here rather than trusted to survive the trip.
  tmux_on_target() { remote "tmux $(printf '%q ' "$@")"; }
else
  remote() { bash -lc "$*"; }
  tmux_on_target() { tmux -L "$socket" "$@"; }
fi

control_pid=""
daemon_pid=""
cleanup() {
  [[ -n "$control_pid" ]] && kill "$control_pid" 2>/dev/null || true
  tmux_on_target kill-session -t "$session" >/dev/null 2>&1 || true
  if [[ -z "$target" ]]; then
    ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
      "$host_binary" daemon-stop >/dev/null 2>&1 || true
    [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null || true
    tmux -L "$socket" kill-server >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

cd "$repo_root"
cargo build --release --bin muxflow-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1

tmux_on_target new-session -d -x "$bound_columns" -y "$bound_rows" -s "$session" "exec bash --norc"

# The stand-in for the user's plain terminal: a second client attached to the
# same session that declares a fixed size. tmux sizes a window for the clients
# looking at it, so this is what makes "did the app grow somebody else's window"
# an observable question at all.
#
# Its stdin stays open for the whole run. A control client whose stdin reaches
# EOF detaches immediately, and a detached stand-in silently turns this lane
# into a session nobody else is looking at — the census below is what caught
# that.
stand_in_stdin="$runtime/stand-in.fifo"
mkfifo "$stand_in_stdin"
if [[ -n "$target" ]]; then
  ssh -F "$ssh_config" -o BatchMode=yes -o ConnectTimeout=15 "$target" \
    "tmux -C attach-session -t $session" \
    <"$stand_in_stdin" >"$runtime/control-client.log" 2>&1 &
else
  tmux -L "$socket" -C attach-session -t "$session" \
    <"$stand_in_stdin" >"$runtime/control-client.log" 2>&1 &
fi
control_pid=$!
# This script itself is the writer, so the client's stdin stays open exactly as
# long as the run does and no helper process outlives it. Opening a FIFO for
# writing blocks until a reader arrives, so a stand-in that failed to start
# would hang the lane here forever rather than failing it.
for _ in $(seq 1 50); do
  [[ -n "$(pgrep -P $$ -f 'attach-session' 2>/dev/null || true)" ]] && break
  kill -0 "$control_pid" 2>/dev/null || { echo "the fixed-size stand-in client exited before it attached" >&2; cat "$runtime/control-client.log" >&2; exit 1; }
  sleep 0.1
done
exec 9>"$stand_in_stdin"
printf 'refresh-client -C %s,%s\n' "$bound_columns" "$bound_rows" >&9
sleep 2

if [[ -z "$target" ]]; then
  ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
    "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
  daemon_pid=$!
  sleep 1
  kill -0 "$daemon_pid"
  transport=(--transport local --host-binary "$host_binary" --runtime "$runtime" --tmux-socket "$socket")
  connections_before=$(jq -r '.counters.connectionsAccepted // 0' "$runtime/diagnostics.json" 2>/dev/null || echo 0)
  rejections_before=$(jq -r '.counters.terminalClientResizeRejections // 0' "$runtime/diagnostics.json" 2>/dev/null || echo 0)
else
  transport=(--transport ssh --ssh-config "$ssh_config" --ssh-target "$target")
  remote "cat $remote_runtime/diagnostics.json" >"$runtime/diagnostics-before.json"
  connections_before=$(jq -r '.counters.connectionsAccepted' "$runtime/diagnostics-before.json")
  rejections_before=$(jq -r '.counters.terminalClientResizeRejections // 0' "$runtime/diagnostics-before.json")
fi

# Proof the stand-in was there, on both sides of the run. Without it the lane
# would be measuring a session nobody else is looking at, which is not the
# situation that damaged the user's windows.
clients() { tmux_on_target list-clients -t "$session" -F '#{client_name} #{client_width}x#{client_height}'; }
clients_before="$(clients | tr '\n' ';')"

ADE_PHASE1_TESTING=1 "$driver_binary" client-geometry \
  "${transport[@]}" \
  --primary "$session" \
  --columns "$columns" --rows "$rows" \
  --bound-columns "$bound_columns" --bound-rows "$bound_rows" \
  --churn-rounds "$churn_rounds" \
  --idle-seconds "$idle_seconds" \
  --label "client-geometry-${target:-local}-${idle_seconds}s" \
  >"$runtime/client-geometry.json"

if [[ -z "$target" ]]; then
  counters_after="$runtime/diagnostics.json"
  connections_after=$(jq -r '.counters.connectionsAccepted // 0' "$counters_after" 2>/dev/null || echo 0)
  rejections_after=$(jq -r '.counters.terminalClientResizeRejections // 0' "$counters_after" 2>/dev/null || echo 0)
else
  remote "cat $remote_runtime/diagnostics.json" >"$runtime/diagnostics-after.json"
  connections_after=$(jq -r '.counters.connectionsAccepted' "$runtime/diagnostics-after.json")
  rejections_after=$(jq -r '.counters.terminalClientResizeRejections // 0' "$runtime/diagnostics-after.json")
fi
# One connection is this run's own bridge. Anything beyond that during an idle
# window is something reconnecting on its own, which is what §12.10 item 4 asks
# about after ~72 connections appeared in one short user session.
connection_delta=$((connections_after - connections_before))
rejection_delta=$((rejections_after - rejections_before))
clients_after="$(clients | tr '\n' ';')"
jq --argjson delta "$connection_delta" \
  --argjson before "$connections_before" --argjson after "$connections_after" \
  --argjson rejections "$rejection_delta" \
  --arg clientsBefore "$clients_before" --arg clientsAfter "$clients_after" \
  '. + {connectionsAcceptedBefore: $before, connectionsAcceptedAfter: $after, connectionsAcceptedDelta: $delta,
        resizeRejectionsRecordedByHost: $rejections,
        attachedClientsBefore: $clientsBefore, attachedClientsAfter: $clientsAfter}' \
  "$runtime/client-geometry.json" >"$runtime/client-geometry-final.json"
mv "$runtime/client-geometry-final.json" "$runtime/client-geometry.json"
cat "$runtime/client-geometry.json"

# Width only: tmux 3.7b reports no `client_height` for a control client, while
# the size it was given is plainly visible in the panes it sizes.
jq -e --arg stand_in "${bound_columns}x" '
  # Nothing on the server ever grew past the terminal a human is looking at,
  # and that terminal was attached the whole time.
  (.attachedClientsBefore | contains($stand_in))
  and (.attachedClientsAfter | contains($stand_in))
  and (.windowsExceedingBound | length) == 0
  # Every size this lane asked for was accepted; a rejection means the fixed
  # size itself is outside the host bound.
  and (.resizeRejections | length) == 0
  # Idle means idle: no topology pushes ("topology changed"), no reseeds, no
  # connection-wide resyncs, and no window moving on its own.
  and .idleTopologyDirty == 0 and .idleTopologySnapshots == 0
  and .idleTerminalSeeds == 0 and .idleConnectionWideResyncs == 0
  and ([.idleSamples[].windows] | unique | length) <= 1
  and (.sequenceGapObserved | not)
  and .connectionsAcceptedDelta <= 1
  # A size no display has is refused by the host, by name, and does not reach
  # `refresh-client -C` (§12.10 item 2, proved by fault injection).
  and (.boundProbe.accepted | not) and .boundProbe.namesTheSize
  # The host counted the refusal it logged, so the bound is observable after the
  # fact and not only in the error string returned to the caller.
  and .resizeRejectionsRecordedByHost >= 1
' "$runtime/client-geometry.json" >/dev/null || {
  echo "FAIL  client-geometry  see windowsExceedingBound / idle counters above" >&2
  echo "artifacts: $runtime" >&2
  exit 1
}
printf 'PASS  client-geometry  no window exceeded %sx%s; %ss idle with 0 topology pushes and %s new connections; %s identical resends cost %s topology pushes\n' \
  "$bound_columns" "$bound_rows" "$idle_seconds" "$connection_delta" \
  "$(jq -r '.identicalResend.requests' "$runtime/client-geometry.json")" \
  "$(jq -r '.identicalResend.topologyDirty' "$runtime/client-geometry.json")"
printf 'artifacts: %s\n' "$runtime"
