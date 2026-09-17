#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../../.." && pwd -P)
[[ $(uname -s) == Darwin ]] || { echo "physical macOS is required" >&2; exit 69; }
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
work="$repo/tmp/work/phase10/local-$run_id"
runtime="$work/runtime"
socket="p10-$$"
host="$repo/target/debug/muxflow-host"
export TMUX_TMPDIR="$repo/tmp/t10-$$"

cleanup() {
  ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
    "$host" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$socket" kill-server >/dev/null 2>&1 || true
  [[ "$work" == "$repo/tmp/work/phase10/local-"* ]] && rm -rf "$work"
  [[ "$TMUX_TMPDIR" == "$repo/tmp/t10-"* ]] && rm -rf "$TMUX_TMPDIR"
}
trap cleanup EXIT

mkdir -p "$runtime" "$work/repo" "$TMUX_TMPDIR"
chmod 0700 "$work" "$runtime" "$TMUX_TMPDIR"
cd "$repo"
cargo build --locked -p muxflow-host
cargo test --locked -p muxflow --lib connection::files::local_destination
cargo test --locked -p muxflow --lib connection::files::clipboard_staging
cargo test --locked -p muxflow-host service::agents::process::tests

tmux -L "$socket" -f /dev/null new-session -d -s "ade-phase10-$run_id" -c "$work/repo"
ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" "$host" discover \
  >"$work/discover.json"
jq -e '.sessions | length >= 1' "$work/discover.json" >/dev/null

git -C "$work/repo" init -q
git -C "$work/repo" config user.name 'Phase 10 Fixture'
git -C "$work/repo" config user.email 'phase10@example.invalid'
git -C "$work/repo" config core.quotepath false
printf 'phase10\n' >"$work/repo/NFC-é.txt"
git -C "$work/repo" add -- 'NFC-é.txt'
git -C "$work/repo" commit -qm baseline
git -C "$work/repo" mv -- 'NFC-é.txt' 'case-É.txt'
git -C "$work/repo" status --porcelain=v1 | rg '^R ' >/dev/null

ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host" daemon >"$work/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 100); do
  ADE_HOST_RUNTIME_DIR="$runtime" "$host" protocol-check >/dev/null 2>&1 && break
  sleep 0.05
done
ADE_HOST_RUNTIME_DIR="$runtime" "$host" protocol-check | jq -e '.protocolMajor != null' >/dev/null
[[ $(stat -f '%Lp' "$runtime") == 700 ]]
ADE_HOST_RUNTIME_DIR="$runtime" "$host" daemon-stop >/dev/null
wait "$daemon_pid"

echo "PHASE10_LOCAL_MAC_SMOKE_PASS"
