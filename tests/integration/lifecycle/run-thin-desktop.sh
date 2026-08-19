#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
stage="$repo_root/tmp/phase1-thin-app/bin"
runtime="$repo_root/tmp/phase1-thin-app/runtime"
tmux_socket="ade-phase1-thin-$$"
desktop_pid=""

cleanup() {
  if [[ -n "$desktop_pid" ]]; then
    kill "$desktop_pid" >/dev/null 2>&1 || true
    wait "$desktop_pid" >/dev/null 2>&1 || true
  fi
  tmux -L "$tmux_socket" kill-server >/dev/null 2>&1 || true
}
trap cleanup EXIT

cd "$repo_root"
cargo build --release --bin tmux-ide-host
pnpm tauri build --no-bundle
mkdir -p "$stage"
release_target="${CARGO_TARGET_DIR:-$repo_root/target}/release"
cp "$release_target/tmux-agent-desktop" "$stage/tmux-agent-desktop"
cp "$release_target/tmux-ide-host" "$stage/tmux-ide-host"
chmod 0755 "$stage/tmux-agent-desktop" "$stage/tmux-ide-host"

metadata="$($stage/tmux-ide-host version)"
jq -e '.helperVersion == "0.2.0" and .protocolMajor == 2' <<<"$metadata" >/dev/null
[[ -x "$stage/tmux-agent-desktop" ]]
[[ -x "$stage/tmux-ide-host" ]]
[[ -n "${DISPLAY:-}" ]] || { echo 'phase1 thin desktop smoke requires DISPLAY' >&2; exit 1; }
rm -rf "$runtime"
mkdir -p "$runtime/config" "$runtime/data" "$runtime/cache" "$runtime/host"
tmux -L "$tmux_socket" new-session -d -s thin 'bash'
env \
  GDK_BACKEND=x11 \
  XDG_CONFIG_HOME="$runtime/config" \
  XDG_DATA_HOME="$runtime/data" \
  XDG_CACHE_HOME="$runtime/cache" \
  ADE_HOST_RUNTIME_DIR="$runtime/host" \
  ADE_TMUX_SOCKET_NAME="$tmux_socket" \
  "$stage/tmux-agent-desktop" >"$runtime/desktop.log" 2>&1 &
desktop_pid=$!
for _ in $(seq 1 200); do
  [[ -S "$runtime/host/host.sock" ]] && break
  kill -0 "$desktop_pid" 2>/dev/null || { cat "$runtime/desktop.log" >&2; exit 1; }
  sleep 0.05
done
[[ -S "$runtime/host/host.sock" ]]
"$stage/tmux-ide-host" protocol-check --socket "$runtime/host/host.sock" \
  | jq -e '.helperVersion == "0.2.0" and .protocolMajor == 2' >/dev/null
echo "phase1-thin-desktop: pass ($stage)"
