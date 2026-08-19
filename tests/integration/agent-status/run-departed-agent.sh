#!/usr/bin/env bash
#
# Phase 14: a killed agent's row disappears, with nothing connected.
#
# This is the lane that covers what screen scraping used to be asked to do and
# could not. The agent is a real process in a real tmux pane, detected the way
# production detects one; a real hook marks it working; the process is killed;
# and the assertion is the daemon's own persisted store, which is exactly what
# the app reads on connect.
#
# The two properties under test are both invisible to a unit test:
#
#   1. No desktop is ever connected. The old sweep sat behind
#      `if !self.subscribed { continue; }` and behind the snapshot request, so a
#      daemon left alone accrued Working states nothing could withdraw. Here the
#      daemon's own timer has to do it.
#   2. The tmux topology never changes. The pane outlives the agent, so
#      `reconcile_topology` — which only runs on a changed snapshot — never
#      fires. Retirement has to come from process evidence alone.
#
# Isolation is not optional: daemon, runtime directory, store and tmux server
# are all created for this run and destroyed with it.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
helper=${ADE_HELPER:-"$here/../../target/debug/tmux-ide-host"}
run_id=$$
socket_name="ade-phase14-$run_id"
runtime=$(mktemp -d "${TMPDIR:-/tmp}/ade14.XXXXXX")
session="ade-phase14-$run_id"

cleanup() {
  "$helper" daemon-stop --socket "$runtime/host.sock" >/dev/null 2>&1 || true
  tmux -L "$socket_name" kill-server 2>/dev/null || true
  rm -rf "$runtime"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
[[ -x "$helper" ]] || fail "helper not built at $helper"

export ADE_HOST_RUNTIME_DIR="$runtime"
export ADE_TMUX_SOCKET_NAME="$socket_name"

# `node <path>/codex`, which is how both real agents actually run and the exact
# shape the process-tree walk exists for. It matters that this is the *hard*
# detection path: the pane's own `current_command` reads "node", so nothing
# about the pane changes when the agent dies, and `reconcile_topology` — which
# only runs on a changed tmux snapshot — never fires. Retirement has to come
# from walking the tree.
command -v node >/dev/null || fail "node is required to stand in for an agent process"
fake_agent="$runtime/bin/codex"
mkdir -p "$runtime/bin"
printf 'setTimeout(() => {}, 600000)\n' >"$fake_agent"

tmux -L "$socket_name" new-session -d -s "$session" -x 100 -y 30
echo "tmux: $(tmux -V)"

"$helper" daemon --socket "$runtime/host.sock" >"$runtime/daemon.log" 2>&1 &
for _ in $(seq 1 50); do
  [[ -S "$runtime/host.sock" ]] && break
  sleep 0.1
done
[[ -S "$runtime/host.sock" ]] || { cat "$runtime/daemon.log"; fail "daemon did not start"; }

pane=$(tmux -L "$socket_name" list-panes -t "$session" -F '#{pane_id}' | head -1)
store="$runtime/agents.json"
field() { "$here/store-field.sh" "$store" "$1"; }
# `grep -c` exits 1 on zero matches, which under `pipefail` reads as a script
# failure rather than the answer "none". Zero records is the expected end state
# here, so it has to be a value, not an error.
records() {
  [[ -f "$store" ]] || { echo 0; return; }
  local count
  # `grep -c` prints the count *and* exits 1 when it is zero, so the exit
  # status has to be discarded rather than turned into a second answer.
  count=$(grep -c '"agent_id"' "$store" 2>/dev/null) || true
  echo "${count:-0}"
}

echo "== an agent is running =="
# Wait for the shell to actually be up. send-keys into a pane whose shell has
# not finished starting is silently dropped, and the failure then looks like a
# detection bug rather than a race in the harness.
for _ in $(seq 1 100); do
  [[ -n "$(tmux -L "$socket_name" display-message -p -t "$pane" '#{pane_current_command}')" ]] \
    && [[ "$(tmux -L "$socket_name" display-message -p -t "$pane" '#{pane_current_command}')" != "" ]] && break
  sleep 0.1
done
sleep 1
# Deliberately *not* `exec`: the pane's shell has to outlive the agent, because
# a pane that closes with its agent is the easy case that reconciliation
# already handles. The case this phase exists for is the pane that stays.
tmux -L "$socket_name" send-keys -t "$pane" "node '$fake_agent'" Enter
for _ in $(seq 1 100); do
  # `|| true` because pgrep exits 1 when it finds nothing, and `pipefail`
  # would take that as a script failure rather than "not up yet".
  agent_pid=$(pgrep -f "node $fake_agent" | head -1 || true)
  [[ -n "$agent_pid" ]] && break
  sleep 0.1
done
[[ -n "$agent_pid" ]] || fail "the agent process never started"
shell_pid=$(tmux -L "$socket_name" display-message -p -t "$pane" '#{pane_pid}')
current=$(tmux -L "$socket_name" display-message -p -t "$pane" '#{pane_current_command}')
[[ "$current" != "codex" ]] || fail "this lane is meant to exercise the tree walk, not basename detection"
echo "  pane $pane reads '$current'; the agent is pid $agent_pid under shell $shell_pid"

echo "== a hook says it is working =="
printf '%s' '{"hook_event_name":"UserPromptSubmit","session_id":"phase14-native"}' \
  | env TMUX_PANE="$pane" TMUX="$(tmux -L "$socket_name" display-message -p '#{socket_path},0,0')" \
    "$helper" hook ingest --adapter codex
[[ "$(field lifecycle)" == "working" ]] || fail "lifecycle was '$(field lifecycle)', expected 'working'"
echo "  lifecycle = working, records = $(records)"

echo "== the daemon leaves a live agent alone =="
sleep 5
[[ "$(records)" == "1" ]] || fail "a live agent's row was retired"
[[ "$(field lifecycle)" == "working" ]] || fail "a live agent stopped reading working"
echo "  after 5s of maintenance passes: still working, records = $(records)"

echo "== the agent is killed; the pane survives it =="
killed=$(date +%s%N)
kill -9 "$agent_pid" 2>/dev/null || true
for _ in $(seq 1 100); do
  [[ "$(records)" == "0" ]] && break
  sleep 0.2
done
retired=$(date +%s%N)
[[ "$(records)" == "0" ]] || fail "the row survived the process by more than 20s (records=$(records))"
kill -0 "$shell_pid" 2>/dev/null || fail "the pane died too; this proved the easy case, not the hard one"
echo "  retired $(( (retired - killed) / 1000000 )) ms after the kill, with no desktop connected"
echo "  the pane is still alive, so no tmux change carried this"

echo "PASS: a departed agent's row disappears"
