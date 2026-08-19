#!/usr/bin/env bash
#
# Phase 13.6.1/13.6.2: the app's own surfaces, driven against a real daemon.
#
# Builds on the Phase 12 fixture — isolated HOME, isolated runtime, isolated
# stock tmux server, its own bundle — and adds the one thing this phase needs:
# a `~/.claude/settings.json` inside that fixture HOME whose every hook belongs
# to somebody else. That is the field machine's configuration, so the app comes
# up in exactly the state the user was in: an agent detected, no hook of ours
# wired, and nothing able to say what the agent is doing.
#
# Nothing here touches a real `~/.claude`, a real tmux server, or the daemon the
# user's own app is talking to. Prints the run directory; `<run>/launch.sh`
# starts the app, `<run>/cleanup.sh` removes everything.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$here/../../.." && pwd)

run_root=$(bash "$repo_root/tests/performance/runtime/setup-qa.sh" | tail -1)
home="$run_root/home"

mkdir -p "$home/.claude"
cp "$here/fixtures/claude-settings-orca.json" "$home/.claude/settings.json"
chmod 0600 "$home/.claude/settings.json"

# An agent process for the app to detect, in the fixture's own tmux server, so
# the agents list has a row whose state nothing can report.
socket=$(grep -o "ADE_TMUX_SOCKET_NAME='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
tmux_tmpdir=$(grep -o "TMUX_TMPDIR='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
helper=$(grep -o "ADE_HOST_HELPER_PATH='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)
mkdir -p "$run_root/bin"
# A process genuinely named `claude`, so the daemon's process detection finds
# it. `hook ingest` blocks reading stdin and contacts nothing; borrowing the
# helper is the only portable way to get a real process under that name.
cp "$helper" "$run_root/bin/claude"

T() { env -u TMUX TMUX_TMPDIR="$tmux_tmpdir" tmux -L "$socket" -f /dev/null "$@"; }
# The same session `launch.sh` would create, made here so the agent window can
# join it; `launch.sh` leaves an existing server alone.
T new-session -d -s phase12 -c "$run_root/repository" 'exec bash'
T new-window -d -t phase12 -n second -c "$run_root/repository" 'exec bash'
T new-window -d -t phase12 -n agent "$run_root/bin/claude hook ingest --adapter claude-code"

echo "$run_root"
