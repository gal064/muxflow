#!/usr/bin/env bash
#
# Phase 13.3/13.6.3: what happened while nothing was watching.
#
# Two absences, not one:
#
#   daemon up, app away    the ordinary case — hooks reach the daemon over its
#                          socket and land in the store; the snapshot a
#                          reconnecting app reads has to carry them, unread
#   daemon down            hooks have nowhere to send, so they write the
#                          fallback mailbox; a daemon started later must ingest
#                          it and reach the same state
#
# Both are asserted against the daemon's persisted store, which is the thing a
# reconnecting desktop actually reads.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
helper=${ADE_HELPER:-"$here/../../target/debug/muxflow-host"}
run_id=$$
socket_name="ade-phase13cu-$run_id"
runtime=$(mktemp -d "${TMPDIR:-/tmp}/ade13cu.XXXXXX")
session="ade-phase13cu-$run_id"

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
store="$runtime/agents.json"
field() { "$here/store-field.sh" "$store" "$1"; }

tmux -L "$socket_name" new-session -d -s "$session" -x 100 -y 30
pane=$(tmux -L "$socket_name" list-panes -t "$session" -F '#{pane_id}' | head -1)

start_daemon() {
  "$helper" daemon --socket "$runtime/host.sock" >>"$runtime/daemon.log" 2>&1 &
  for _ in $(seq 1 50); do
    [[ -S "$runtime/host.sock" ]] && return 0
    sleep 0.1
  done
  cat "$runtime/daemon.log"
  fail "daemon did not start"
}

# A real hook inherits TMUX and TMUX_PANE from the pane it fires in; the pane
# ID is server-local, so without the server identity the daemon is right to
# refuse to route it anywhere.
tmux_env="$(tmux -L "$socket_name" display-message -p '#{socket_path},0,0')"
send() {
  printf '{"hook_event_name":"%s","session_id":"phase13-native"}' "$1" \
    | env TMUX_PANE="$pane" TMUX="$tmux_env" "$helper" hook ingest --adapter claude-code
}

echo "== daemon up, no app watching =="
start_daemon
send UserPromptSubmit
send Stop
[[ "$(field lifecycle)" == "idle" ]] || fail "turn did not finish while the app was away"
[[ "$(field attention_kind)" == "completed" ]] || fail "the finished turn left no attention"
[[ "$(field attention_generation)" -gt "$(field seen_generation)" ]] \
  || fail "the finished turn is not unread"
first_attention=$(field attention_generation)
echo "  unread after the gap: attention $first_attention > seen $(field seen_generation)"

echo "== daemon down =="
"$helper" daemon-stop --socket "$runtime/host.sock" >/dev/null 2>&1 || true
sleep 0.5
send UserPromptSubmit
send PermissionRequest
mailbox=$(find "$runtime" -maxdepth 1 -name "hook-fallback-*.pb" | wc -l | tr -d " ")
[[ "$mailbox" -ge 1 ]] || fail "hooks fired at a stopped daemon left nothing behind"
echo "  fallback mailbox holds $mailbox event(s)"
[[ "$(field lifecycle)" == "idle" ]] || fail "the store changed with no daemon running"

echo "== daemon back =="
start_daemon
for _ in $(seq 1 50); do
  if [[ "$(field lifecycle)" == "blocked" ]]; then break; fi
  sleep 0.1
done
[[ "$(field lifecycle)" == "blocked" ]] || fail "the restarted daemon did not ingest the mailbox"
[[ "$(field attention_kind)" == "blocked" ]] || fail "the blocked agent left no attention"
[[ "$(field attention_generation)" -gt "$(field seen_generation)" ]] || fail "the block is not unread"
[[ "$(field attention_generation)" -gt "$first_attention" ]] \
  || fail "attention did not advance past what was already there"
remaining=$(find "$runtime" -maxdepth 1 -name "hook-fallback-*.pb" | wc -l | tr -d " ")
[[ "$remaining" == "0" ]] || fail "$remaining mailbox events were left behind"
echo "  caught up: attention $(field attention_generation) > seen $(field seen_generation), mailbox drained"

echo "PASS: catch-up after a disconnect"
