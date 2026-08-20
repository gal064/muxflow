#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase7 staging-parent swap gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
evidence="$repo_root/tmp/phase7-parent-swap-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/transfers/protocol-driver/target}"
host_binary="$main_target/debug/muxflow-host"
driver_binary="$driver_target/debug/transfer-test-driver"
socket_name="ade-phase7-swap-$$"
host_runtime="$evidence/host-runtime"
fixture_home="$evidence/home"

cleanup() {
  HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$socket_name" kill-server >/dev/null 2>&1 || true
}
trap cleanup EXIT
mkdir -p "$evidence" "$host_runtime" "$fixture_home/workspace"
source "$repo_root/tests/integration/transfers/source-tree-evidence.sh"
phase7_capture_source_tree "$repo_root" "$evidence"
chmod 0700 "$evidence" "$host_runtime" "$fixture_home"
printf '%s\n' "$evidence" >"$repo_root/tmp/phase7-parent-swap-latest"
cd "$repo_root"
cargo build --bin muxflow-host >"$evidence/host-build.log" 2>&1 & host_pid=$!
cargo build --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml \
  >"$evidence/driver-build.log" 2>&1 & driver_pid=$!
phase7_wait_all "$host_pid" "$driver_pid"
tmux -L "$socket_name" new-session -d -s phase7-swap -c "$fixture_home/workspace" 'exec bash'
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
ADE_PHASE7_STAGING_DIR="$fixture_home/.cache/muxflow/uploads" \
  "$driver_binary" parent-swap local "$host_binary" \
  >"$evidence/result.json" 2>"$evidence/driver.log" & swap_pid=$!
wait "$swap_pid"
phase7_embed_source_digest "$evidence" "$evidence/result.json"
jq -e '.commitFailedClosed and .commitOutcome == "notPublished" and
  (.attackerDestinationPublished | not) and
  (.backupDestinationPublished | not) and (.partialLeakedAfterParentSwap | not) and
  .reconciliationManifestAbsent and .stagingRecovered and .parentSwapSafe' \
  "$evidence/result.json" >/dev/null
phase7_assert_source_tree_unchanged "$repo_root" "$evidence"
printf '%s\n' "phase7 staging-parent swap evidence: $evidence"
