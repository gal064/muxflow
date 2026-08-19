#!/usr/bin/env bash
#
# Phase 13.6.2: hook event → what the app shows, measured against the running
# app rather than against the daemon's store.
#
# The clock starts before the `hook ingest` process is spawned and stops when
# the agents list reports the new state, read out of the app's accessibility
# tree. That makes it an *upper* bound: one accessibility snapshot is itself
# tens to hundreds of milliseconds, and the poll interval is charged to the
# measurement too. The gate is < 1s, and an upper bound is the honest side to
# be wrong on.
#
# Usage: measure-ui-latency.sh <run-root> <pid> <window-id>
set -euo pipefail

run_root=$1
pid=$2
window=$3
here=$(cd "$(dirname "$0")" && pwd)

socket=$(grep -o "ADE_TMUX_SOCKET_NAME='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
tmux_tmpdir=$(grep -o "TMUX_TMPDIR='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
runtime=$(grep -o "XDG_RUNTIME_DIR='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
helper=$(grep -o "ADE_HOST_HELPER_PATH='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)

T() { env -u TMUX TMUX_TMPDIR="$tmux_tmpdir" tmux -L "$socket" "$@"; }
pane=$(T list-panes -a -F '#{window_name} #{pane_id}' | awk '$1=="agent"{print $2}')
tmux_env="$(T display-message -p '#{socket_path},0,0')"
[[ -n "$pane" ]] || { echo "no agent pane in the fixture" >&2; exit 1; }

# The app's own daemon, in the fixture's runtime directory.
export ADE_HOST_RUNTIME_DIR="$runtime/tmux-agent-ide"
[[ -S "$ADE_HOST_RUNTIME_DIR/host.sock" ]] || ADE_HOST_RUNTIME_DIR="$runtime"

agent_state() {
  cua-driver call get_window_state \
    "{\"pid\":$pid,\"window_id\":$window,\"include_screenshot\":false,\"query\":\"Claude Code\"}" \
    2>/dev/null | grep -o '"label": *"Claude Code, [a-z]*' | head -1 | sed 's/.*, //'
}

step() {
  local event=$1 expected=$2 started ended
  started=$(date +%s%N)
  printf '{"hook_event_name":"%s","session_id":"phase13-ui"}' "$event" \
    | env TMUX_PANE="$pane" TMUX="$tmux_env" "$helper" hook ingest --adapter claude-code
  for _ in $(seq 1 60); do
    if [[ "$(agent_state)" == "$expected" ]]; then
      ended=$(date +%s%N)
      echo "  $event -> the agents list says '$expected' within $(( (ended - started) / 1000000 )) ms"
      return 0
    fi
  done
  echo "FAIL: after $event the agents list said '$(agent_state)', expected '$expected'" >&2
  exit 1
}

echo "agent pane: $pane"
echo "before: the agents list says '$(agent_state)'"
step UserPromptSubmit working
step PermissionRequest blocked
step PostToolUse working
step Stop done
echo "PASS: every transition reached the app's own surface"

# One accessibility snapshot of this window costs ~390-420 ms on this machine
# (measured separately), and every figure above contains at least one. The
# daemon-side half is measured by `run-agent-status.sh`, which times the
# `hook ingest` process itself: 12-13 ms on the field machine, 80-110 ms on
# macOS. Subtracting one snapshot from each figure above leaves ~110-180 ms of
# app-side work.
