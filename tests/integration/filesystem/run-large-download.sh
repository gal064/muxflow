#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase4 large-download QA failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
source "$repo_root/tests/release/storage.sh"
phase8_storage_begin "$repo_root" phase4-large-download
runtime="$PHASE8_WORK_DIR"
evidence="$PHASE8_EVIDENCE_DIR"
host_binary="$CARGO_TARGET_DIR/debug/muxflow-host"
driver_binary="$CARGO_TARGET_DIR/debug/filesystem-test-driver"
socket_name="ade-phase4-large-$$"
host_runtime="$runtime/host-runtime"
fixture="$runtime/repository"

cleanup() {
  local status=$?
  ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  for _ in $(seq 1 100); do
    [[ ! -e "$host_runtime/host.sock" && ! -e "$host_runtime/daemon.json" ]] && break
    sleep 0.02
  done
  tmux -L "$socket_name" kill-server >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

mkdir -p "$runtime" "$host_runtime" "$fixture/subdir"
chmod 0700 "$runtime" "$host_runtime"

cd "$repo_root"
cargo build --bin muxflow-host >"$runtime/cargo-host.log" 2>&1 &
host_build_pid=$!
cargo build --manifest-path tests/integration/filesystem/protocol-driver/Cargo.toml \
  >"$runtime/cargo-driver.log" 2>&1 &
driver_build_pid=$!
wait "$host_build_pid"
wait "$driver_build_pid"

git -C "$fixture" init -q
truncate -s 5368709120 "$fixture/five-gib.bin"
source_size="$(phase8_stat_size "$fixture/five-gib.bin")"
source_allocated="$(phase8_dir_bytes "$fixture/five-gib.bin")"
[[ "$source_size" == 5368709120 ]]
tmux -L "$socket_name" new-session -d -s phase4-large -c "$fixture/subdir" 'exec bash'

ADE_HOST_RUNTIME_DIR="$host_runtime" \
ADE_TMUX_SOCKET_NAME="$socket_name" \
ADE_PHASE4_LARGE_FILE=five-gib.bin \
  "$driver_binary" large-local "$host_binary" \
  >"$runtime/result.json" 2>"$runtime/driver.log"

jq -e '
  .downloadBytes == "5368709120" and
  .exceedsU32 and
  .blake3Verified and
  .chunkBytesMax == 1048576 and
  .controlProbes > 0 and
  .maxControlLatencyMs < 1000 and
  .cancelledConcurrentTransfer and
  .cancelledTransferReleased and
  .driverHwmKiB < .memoryLimitKiB and
  .daemonHwmKiB < .memoryLimitKiB
' "$runtime/result.json" >/dev/null

jq -n \
  --arg artifact "$evidence" \
  --arg sourceBytes "$source_size" \
  --arg allocatedBytes "$source_allocated" \
  '{artifact: $artifact, sourceBytes: $sourceBytes, allocatedBytes: $allocatedBytes}' \
  >"$runtime/fixture.json"
cp "$runtime/result.json" "$runtime/fixture.json" "$runtime/driver.log" "$evidence/"
phase8_storage_publish "$repo_root" phase4-large-download-latest
printf '%s\n' "phase4 5 GiB download evidence: $evidence"
