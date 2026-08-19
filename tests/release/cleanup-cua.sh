#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
run_root=$(realpath "$repo_root/tmp/phase8-cua-latest")
home="$run_root/home"
runtime="$run_root/runtime"
tmux_runtime=$(cat "$run_root/tmux-runtime" 2>/dev/null || printf '%s\n' "$runtime")
prefix="$run_root/prefix"
container=$(cat "$run_root/container-name")
tmux_socket=$(cat "$run_root/tmux-socket-name")

pkill -f "$prefix/lib/tmux-agent-ide/tmux-agent-desktop" >/dev/null 2>&1 || true
HOME="$home" ADE_HOST_RUNTIME_DIR="$runtime" "$prefix/lib/tmux-agent-ide/tmux-ide-host" daemon-stop >/dev/null 2>&1 || true
env -u TMUX HOME="$home" XDG_RUNTIME_DIR="$runtime" TMUX_TMPDIR="$tmux_runtime" tmux -L "$tmux_socket" kill-server >/dev/null 2>&1 || true
env -u TMUX HOME="$home" XDG_RUNTIME_DIR="$runtime" TMUX_TMPDIR="$tmux_runtime" tmux -L phase8-inner kill-server >/dev/null 2>&1 || true
docker rm -f "$container" >/dev/null 2>&1 || true
cua-virtual-driver call end_session '{"session":"phase8-linux-release"}' >/dev/null 2>&1 || true
printf 'Phase 8 CUA processes cleaned; evidence retained at %s\n' "$run_root"
