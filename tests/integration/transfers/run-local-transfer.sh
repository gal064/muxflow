#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase7 local transfer gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
source "$repo_root/tests/release/storage.sh"
transfer_bytes="${ADE_PHASE7_LOCAL_BYTES:-5368709120}"
if [[ "$transfer_bytes" == 5368709120 && "${ADE_PHASE7_EXACT_5GIB_CONFIRM:-}" != release ]]; then
  echo "exact 5 GiB local transfer requires ADE_PHASE7_EXACT_5GIB_CONFIRM=release; use pnpm test:transfers:smoke for routine verification" >&2
  exit 64
fi
phase8_storage_begin "$repo_root" phase7-local
evidence="$PHASE8_WORK_DIR"
durable_evidence="$PHASE8_EVIDENCE_DIR"
host_binary="$CARGO_TARGET_DIR/debug/tmux-ide-host"
driver_binary="$CARGO_TARGET_DIR/debug/transfer-test-driver"
socket_name="ade-phase7-local-$$"
host_runtime="$evidence/host-runtime"
fixture="$evidence/workspace"
fixture_home="$evidence/home"

cleanup() {
  local status=$?
  HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$socket_name" kill-server >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

mkdir -p "$evidence" "$host_runtime" "$fixture" "$fixture_home"
source "$repo_root/tests/integration/transfers/source-tree-evidence.sh"
phase7_capture_source_tree "$repo_root" "$evidence"
chmod 0700 "$evidence" "$host_runtime" "$fixture_home"
cd "$repo_root"

cargo build --bin tmux-ide-host >"$evidence/host-build.log" 2>&1 & host_pid=$!
cargo build --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml \
  >"$evidence/driver-build.log" 2>&1 & driver_pid=$!
cargo test -p tmux-agent-desktop \
  connection::files::manager_acceptance::production_desktop_managers_transfer_exact_bytes_through_canonical_engine \
  --no-run >"$evidence/manager-build.log" 2>&1 & manager_build_pid=$!
phase7_wait_all "$host_pid" "$driver_pid" "$manager_build_pid"

truncate -s "$transfer_bytes" "$fixture/phase7-five-gib-source.bin"
git -C "$fixture" init -q
tmux -L "$socket_name" new-session -d -s phase7-local -c "$fixture" 'exec bash'
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
ADE_PHASE7_BYTES="$transfer_bytes" ADE_PHASE7_DOWNLOAD_FILE=phase7-five-gib-source.bin \
  "$driver_binary" acceptance local "$host_binary" \
  >"$evidence/result.json" 2>"$evidence/driver.log" & acceptance_pid=$!
wait "$acceptance_pid"
phase7_embed_source_digest "$evidence" "$evidence/result.json"

manager_source="$fixture/phase7-manager-source.bin"
manager_download="$evidence/phase7-manager-download.bin"
truncate -s "$transfer_bytes" "$manager_source"
ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
ADE_HOST_HELPER_PATH="$host_binary" ADE_PHASE7_MANAGER_ACCEPTANCE=1 \
ADE_PHASE7_MANAGER_MODE=local ADE_PHASE7_BYTES="$transfer_bytes" \
ADE_PHASE7_MANAGER_UPLOAD_SOURCE="$manager_source" \
ADE_PHASE7_MANAGER_DOWNLOAD_DESTINATION="$manager_download" \
ADE_PHASE7_MANAGER_DOWNLOAD_FILE=phase7-five-gib-source.bin \
ADE_PHASE7_MANAGER_RESULT="$evidence/result-manager.json" \
  cargo test -p tmux-agent-desktop \
    connection::files::manager_acceptance::production_desktop_managers_transfer_exact_bytes_through_canonical_engine \
    -- --exact --nocapture --test-threads=1 \
    >"$evidence/manager-test.log" 2>&1 & manager_pid=$!
wait "$manager_pid"
phase7_embed_source_digest "$evidence" "$evidence/result-manager.json"
jq -e --arg bytes "$transfer_bytes" '
  .uploadBytes == $bytes and .downloadBytes == $bytes and .blake3Verified and
  .exactTwoWorkers and .queuedCancellation and .uploadLifecycleComplete and
  .downloadLifecycleComplete and .canonicalEngineMaxActive == 2 and
  .controlProbes > 0 and .maxControlLatencyMs < 1500 and
  .desktopManagerHwmKiB < 262144 and
  (($bytes | tonumber) != 5368709120 or .desktopManager5GiB)
' "$evidence/result-manager.json" >/dev/null
jq --slurpfile manager "$evidence/result-manager.json" \
  '. + {desktopManager5GiB: $manager[0].desktopManager5GiB,
    desktopManagerEvidence: "result-manager.json"}' \
  "$evidence/result.json" >"$evidence/result-combined.json"
mv "$evidence/result-combined.json" "$evidence/result.json"

jq -e --arg bytes "$transfer_bytes" '
  .uploadBytes == $bytes and .downloadBytes == $bytes and
  .blake3Verified and .independentBulkConnections == 2 and
  .chunkBytesMax <= .boundedChunkBytes and .controlProbes > 0 and
  .maxControlLatencyMs < 1500 and .faultMatrix.controlBodiesRejected and
  .faultMatrix.largeConfirmationRequired and .faultMatrix.basenameTraversalRejected and
  .faultMatrix.pngLimitEnforced and .faultMatrix.shellSensitiveNameOpaque and
  .faultMatrix.collisionFailAndRename and .faultMatrix.twoLaneRenameDistinct and
  .faultMatrix.staleOffsetRejected and
  .faultMatrix.oversizedChunkRejected and .faultMatrix.badDigestCleaned and
  .faultMatrix.cancelCleanedAndReleased and
  (($bytes | tonumber) <= 1073741824 or
    (.crossBridgeCleanupExercised and .crossBridgeCleanupSafe))
' "$evidence/result.json" >/dev/null

final_path="$(jq -r .uploadFinalPath "$evidence/result.json")"
staging="$fixture_home/.cache/tmux-agent-ide/uploads"
phase8_stat_detail "$staging" "$final_path" \
  >"$evidence/staging-stat.txt"
[[ "$(phase8_stat_mode "$staging")" == 700 ]]
[[ "$(phase8_stat_mode "$final_path")" == 600 ]]
[[ "$(phase8_stat_uid "$final_path")" == "$(id -u)" ]]
[[ "$(phase8_stat_size "$final_path")" == "$transfer_bytes" ]]
[[ "$(phase8_stat_size "$manager_download")" == "$transfer_bytes" ]]
if find "$staging" -maxdepth 1 -type f -name '.tmux-agent-upload-*.partial' -print -quit | grep -q .; then
  echo "owned upload partial remained after acceptance" >&2
  exit 1
fi
daemon_pid="$(jq -r .pid "$host_runtime/daemon.json")"
phase8_pid_memory_evidence "$daemon_pid" >"$evidence/daemon-memory.txt"
# This is a generated acceptance artifact; retain its metadata evidence but
# release the 5 GiB allocation so repeated gates do not exhaust the workspace.
truncate -s 0 "$final_path"
manager_upload_path="$(jq -r .uploadFinalPath "$evidence/result-manager.json")"
truncate -s 0 "$manager_upload_path" "$manager_download"
phase7_assert_source_tree_unchanged "$repo_root" "$evidence"
for artifact in result.json result-manager.json source-tree-manifest.tsv source-tree-digest.txt \
  transfer-component-manifest.tsv transfer-component-digest.txt \
  driver.log manager-test.log daemon-memory.txt staging-stat.txt; do
  [[ ! -f "$evidence/$artifact" ]] || cp "$evidence/$artifact" "$durable_evidence/$artifact"
done
phase8_storage_publish "$repo_root" phase7-local-latest
if [[ "$transfer_bytes" == 5368709120 ]]; then
  phase8_storage_publish "$repo_root" phase7-local-5gib-latest
fi
printf '%s\n' "phase7 local transfer evidence: $durable_evidence"
