#!/usr/bin/env bash
#
# Phase 13.2/13.3/13.4b/13.6: the agent-status scenario, end to end, against a
# real daemon and a real tmux server.
#
# What it drives is `hook ingest`, in the order a real Claude Code turn fires
# it, from inside a real tmux pane — so the pane ID, the server identity, the
# daemon connection and the store write are all the production ones. What it
# asserts is the daemon's own persisted store, which is exactly what the app
# reads on connect.
#
# Isolation is not optional here. The daemon, its runtime directory, its store
# and the tmux server are all created for this run and destroyed with it; the
# operator's real tmux servers and real daemon are never touched. Pass
# ADE_STOCK=1 to run the tmux server with `-f /dev/null`, which is the Phase
# 13.4b fresh-machine lane: base-index 0, allow-rename off, mouse off.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
helper=${ADE_HELPER:-"$here/../../target/debug/muxflow-host"}
stock=${ADE_STOCK:-0}
run_id=$$
socket_name="ade-phase13-$run_id"
runtime=$(mktemp -d "${TMPDIR:-/tmp}/ade13.XXXXXX")
session="ade-phase13-$run_id"

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

if [[ "$stock" == "1" ]]; then
  tmux -f /dev/null -L "$socket_name" new-session -d -s "$session" -x 100 -y 30
  echo "lane: stock tmux (-f /dev/null)"
else
  tmux -L "$socket_name" new-session -d -s "$session" -x 100 -y 30
  echo "lane: default tmux config"
fi
echo "tmux: $(tmux -V)"
echo "base-index: $(tmux -L "$socket_name" show-options -gv base-index)"
echo "allow-rename: $(tmux -L "$socket_name" show-options -gv allow-rename 2>/dev/null || echo unset)"

"$helper" daemon --socket "$runtime/host.sock" >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 50); do
  [[ -S "$runtime/host.sock" ]] && break
  sleep 0.1
done
[[ -S "$runtime/host.sock" ]] || { cat "$runtime/daemon.log"; fail "daemon did not start"; }
echo "daemon: pid $daemon_pid on $runtime/host.sock"

pane=$(tmux -L "$socket_name" list-panes -t "$session" -F '#{pane_id}' | head -1)
window=$(tmux -L "$socket_name" list-panes -t "$session" -F '#{window_id}' | head -1)
echo "pane: $pane in window $window"

store="$runtime/agents.json"

# One hook event, sent the way Claude Code sends it: our command on stdin,
# TMUX_PANE in the environment, one process per event.
send() {
  local name=$1 extra=${2:-}
  local payload="{\"hook_event_name\":\"$name\",\"session_id\":\"phase13-native\"$extra}"
  local started ended
  started=$(date +%s%N)
  printf '%s' "$payload" | env TMUX_PANE="$pane" TMUX="$(tmux -L "$socket_name" display-message -p '#{socket_path},0,0')" \
    "$helper" hook ingest --adapter claude-code
  ended=$(date +%s%N)
  echo "  $name -> $(( (ended - started) / 1000000 )) ms"
}

field() {
  "$here/store-field.sh" "$store" "$1"
}

expect() {
  local what=$1 want=$2 got
  got=$(field "$what")
  [[ "$got" == "$want" ]] || fail "$what was '$got', expected '$want'"
  echo "  $what = $got"
}

echo "== working =="
send UserPromptSubmit
expect lifecycle working

echo "== blocked =="
send PermissionRequest
expect lifecycle blocked
expect attention_kind blocked
[[ "$(field attention_generation)" -gt "$(field seen_generation)" ]] || fail "blocked did not become unread"

echo "== working again =="
send PostToolUse
expect lifecycle working

echo "== done, unread =="
send Stop
expect lifecycle idle
expect attention_kind completed
[[ "$(field attention_generation)" -gt "$(field seen_generation)" ]] || fail "the finished turn is not unread"

echo "== routing =="
expect pane_id "$pane"
expect window_id "$window"
expect detected_manually false
# Hooks are the only writer of lifecycle, so there is no source to assert any
# more. What is still worth asserting is the lease the hook took: it is what
# keeps an unmapped record alive and what reconciliation reads before it
# decides a record is stale.
[[ "$(field hook_authority_expires_at_unix_millis)" -gt 0 ]] || fail "the hook took no lease"

echo "== parallel subagents keep the parent working =="
send UserPromptSubmit
expect lifecycle working
attention_before=$(field attention_generation)
send Stop ',"background_tasks":[{"id":"short","type":"subagent","status":"running","description":"private short task"},{"id":"medium","type":"subagent","status":"running","description":"private medium task"},{"id":"long","type":"subagent","status":"running","description":"private long task"}]'
expect lifecycle working
expect hook_terminal false
[[ "$(field attention_generation)" == "$attention_before" ]] || fail "an intermediate parent Stop earned completion attention"
send Notification ',"notification_type":"idle_prompt"'
expect lifecycle working
expect claude_has_running_subagent true
[[ "$(field attention_generation)" == "$attention_before" ]] || fail "an idle notification during subagent work earned blocked attention"
send PermissionRequest
expect lifecycle blocked
blocked_attention=$(field attention_generation)
send Notification ',"notification_type":"idle_prompt"'
expect lifecycle blocked
[[ "$(field attention_generation)" == "$blocked_attention" ]] || fail "a repeated idle notification earned duplicate blocked attention"
send PostToolUse
expect lifecycle working
send SubagentStop ',"agent_id":"short"'
expect lifecycle working
send Stop ',"background_tasks":[{"id":"medium","type":"subagent","status":"running"},{"id":"long","type":"subagent","status":"running"}]'
expect lifecycle working
expect hook_terminal false
send SubagentStop ',"agent_id":"medium"'
expect lifecycle working
send Stop ',"background_tasks":[{"id":"long","type":"subagent","status":"running"}]'
expect lifecycle working
expect hook_terminal false
send SubagentStop ',"agent_id":"long"'
expect lifecycle working
send Stop
expect lifecycle idle
expect hook_terminal true
expect claude_has_running_subagent false
expect attention_kind completed
[[ "$(field attention_generation)" -gt "$attention_before" ]] || fail "the final parent Stop earned no completion attention"
if grep -Fq 'private short task' "$store"; then
  fail "the normalized hook store retained a private subagent description"
fi

echo "== a failed turn also ends the turn =="
send UserPromptSubmit
expect lifecycle working
send StopFailure
expect lifecycle idle

echo "PASS: agent status end to end"
