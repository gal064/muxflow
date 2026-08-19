#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase7 shaped Docker/OpenSSH gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
source "$repo_root/tests/release/storage.sh"
transfer_bytes="${ADE_PHASE7_SSH_BYTES:-5368709120}"
if [[ "$transfer_bytes" == 5368709120 && "${ADE_PHASE7_EXACT_5GIB_CONFIRM:-}" != release ]]; then
  echo "exact 5 GiB SSH transfer requires ADE_PHASE7_EXACT_5GIB_CONFIRM=release; use pnpm test:transfers:smoke for routine verification" >&2
  exit 64
fi
phase8_storage_begin "$repo_root" phase7-ssh
evidence="$PHASE8_WORK_DIR"
durable_evidence="$PHASE8_EVIDENCE_DIR"
host_binary="$CARGO_TARGET_DIR/debug/tmux-ide-host"
driver_binary="$CARGO_TARGET_DIR/debug/transfer-test-driver"
container="ade-phase7-$run_id"
image="tmux-agent-ide-phase7-ssh"
target="ade-phase7-docker"
driver_pid=''
monitor_pid=''
security_pid=''

cleanup() {
  local status=$?
  [[ -n "$monitor_pid" ]] && kill "$monitor_pid" >/dev/null 2>&1 || true
  [[ -n "$driver_pid" ]] && kill "$driver_pid" >/dev/null 2>&1 || true
  [[ -n "$security_pid" ]] && kill "$security_pid" >/dev/null 2>&1 || true
  [[ -f "$evidence/ssh-config" ]] && \
    ssh -F "$evidence/ssh-config" -O exit "$target" >/dev/null 2>&1 || true
  docker rm -f "$container" >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT
command -v docker >/dev/null 2>&1 || { echo "Docker is required" >&2; exit 2; }

# The remote helper's staging cache must stay on the container's own filesystem.
# A macOS bind mount is served by Docker Desktop's shared filesystem, which does
# not enforce flock, so the helper's destination reservations would be invisible
# to one another and two lanes could claim the same name (M10-E039).
mkdir -p "$evidence/docker-config" "$evidence/remote-workspace"
source "$repo_root/tests/integration/transfers/source-tree-evidence.sh"
phase7_capture_source_tree "$repo_root" "$evidence"
chmod 0700 "$evidence" "$evidence/docker-config"
phase8_isolate_docker_config "$evidence/docker-config"
cd "$repo_root"

cargo build --bin tmux-ide-host >"$evidence/host-build.log" 2>&1 & host_pid=$!
cargo build --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml \
  >"$evidence/driver-build.log" 2>&1 & protocol_pid=$!
cargo test -p tmux-agent-desktop \
  connection::files::manager_acceptance::production_desktop_managers_transfer_exact_bytes_through_canonical_engine \
  --no-run >"$evidence/manager-build.log" 2>&1 & manager_build_pid=$!
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
docker build --platform "$docker_platform" -t "$image" tests/integration/transport/ssh-target \
  >"$evidence/docker-build.log" 2>&1 & image_pid=$!
release/linux/build-compatible-host.sh "$target_arch" "$evidence/tmux-ide-host-bookworm" \
  >"$evidence/remote-build.log" 2>&1 & remote_pid=$!
phase7_wait_all "$host_pid" "$protocol_pid" "$manager_build_pid" "$image_pid" "$remote_pid"

ssh-keygen -q -t ed25519 -N '' -f "$evidence/ssh-key"
port=''
for candidate in $(seq 22722 22821); do
  if ! phase8_tcp_port_listening "$candidate"; then port="$candidate"; break; fi
done
[[ -n "$port" ]]
docker run -d --platform "$docker_platform" --name "$container" --cap-add NET_ADMIN \
  -p "127.0.0.1:$port:22" \
  -v "$evidence/ssh-key.pub:/config/authorized_keys:ro" \
  -v "$evidence/remote-workspace:/home/ade/phase7-workspace" \
  "$image" \
  >"$evidence/container-id"
printf '%s\n' \
  "Host $target" \
  '  HostName 127.0.0.1' \
  "  Port $port" \
  '  User ade' \
  "  IdentityFile $evidence/ssh-key" \
  '  IdentitiesOnly yes' \
  '  BatchMode yes' \
  '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' \
  "  UserKnownHostsFile $evidence/known-hosts" \
  '  ControlMaster auto' \
  '  ControlPersist 60' \
  "  ControlPath tmp/p7-$$-%C" >"$evidence/ssh-config"
chmod 0600 "$evidence/ssh-config"
for _ in $(seq 1 100); do
  ssh -F "$evidence/ssh-config" "$target" true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$evidence/ssh-config" "$target" true
ssh -F "$evidence/ssh-config" "$target" 'mkdir -p "$HOME/.local/bin" "$HOME/phase7-workspace"'
scp -q -F "$evidence/ssh-config" "$evidence/tmux-ide-host-bookworm" \
  "$target:/home/ade/.local/bin/tmux-ide-host"
ssh -F "$evidence/ssh-config" "$target" \
  "chmod 0700 \"\$HOME/.local/bin/tmux-ide-host\"; mkdir -p \"\$HOME/phase7-workspace\"; truncate -s '$transfer_bytes' \"\$HOME/phase7-workspace/phase7-five-gib-source.bin\"; git -C \"\$HOME/phase7-workspace\" init -q; tmux new-session -d -s phase7 -c \"\$HOME/phase7-workspace\" 'exec bash'"

# Delay and rate shaping are applied after setup. The one control-master TCP
# connection must remain responsive while exactly two ControlMaster=no bulk
# TCP connections saturate the independent directions.
docker exec "$container" tc qdisc add dev eth0 root netem delay 10ms rate 100mbit
: >"$evidence/tcp-samples.tsv"
ADE_PHASE7_BYTES="$transfer_bytes" ADE_PHASE7_DOWNLOAD_FILE=phase7-five-gib-source.bin \
  "$driver_binary" acceptance ssh "$evidence/ssh-config" "$target" \
  >"$evidence/result.json" 2>"$evidence/driver.log" & driver_pid=$!
(
  while kill -0 "$driver_pid" >/dev/null 2>&1; do
    count="$(docker exec "$container" sh -c "ss -Htn state established '( sport = :22 )' | wc -l" 2>/dev/null || printf 0)"
    printf '%s\t%s\n' "$(date +%s%N)" "$count" >>"$evidence/tcp-samples.tsv"
    sleep 0.1
  done
) & monitor_pid=$!
wait "$driver_pid"
driver_pid=''
wait "$monitor_pid"
monitor_pid=''
phase7_embed_source_digest "$evidence" "$evidence/result.json"

# The protocol driver used the config-defined master. Close it before the
# desktop manager phase so the manager's production per-profile control master
# is the only control TCP lane alongside its two independent bulk lanes.
ssh -F "$evidence/ssh-config" -O exit "$target" >/dev/null 2>&1 || true

manager_source="$evidence/phase7-manager-source.bin"
manager_download="$evidence/phase7-manager-download.bin"
truncate -s "$transfer_bytes" "$manager_source"
ADE_PHASE7_MANAGER_ACCEPTANCE=1 ADE_PHASE7_MANAGER_MODE=ssh \
ADE_PHASE7_MANAGER_SSH_CONFIG="$evidence/ssh-config" \
ADE_PHASE7_MANAGER_SSH_TARGET="$target" ADE_PHASE7_BYTES="$transfer_bytes" \
ADE_PHASE7_MANAGER_UPLOAD_SOURCE="$manager_source" \
ADE_PHASE7_MANAGER_DOWNLOAD_DESTINATION="$manager_download" \
ADE_PHASE7_MANAGER_DOWNLOAD_FILE=phase7-five-gib-source.bin \
ADE_PHASE7_MANAGER_RESULT="$evidence/result-manager.json" \
  cargo test -p tmux-agent-desktop \
    connection::files::manager_acceptance::production_desktop_managers_transfer_exact_bytes_through_canonical_engine \
    -- --exact --nocapture --test-threads=1 \
    >"$evidence/manager-test.log" 2>&1 & driver_pid=$!
(
  while kill -0 "$driver_pid" >/dev/null 2>&1; do
    count="$(docker exec "$container" sh -c "ss -Htn state established '( sport = :22 )' | wc -l" 2>/dev/null || printf 0)"
    printf '%s\t%s\n' "$(date +%s%N)" "$count" >>"$evidence/tcp-samples.tsv"
    sleep 0.1
  done
) & monitor_pid=$!
wait "$driver_pid"
driver_pid=''
wait "$monitor_pid"
monitor_pid=''
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

max_connections="$(awk 'BEGIN { max=0 } $2 > max { max=$2 } END { print max }' "$evidence/tcp-samples.tsv")"
printf '%s\n' "$max_connections" >"$evidence/max-established-ssh-connections.txt"
jq -e --arg bytes "$transfer_bytes" '
  .uploadBytes == $bytes and .downloadBytes == $bytes and .blake3Verified and
  .independentBulkConnections == 2 and .chunkBytesMax <= .boundedChunkBytes and
  .controlProbes > 0 and .maxControlLatencyMs < 1500 and
  .faultMatrix.controlBodiesRejected and .faultMatrix.largeConfirmationRequired and
  .faultMatrix.basenameTraversalRejected and .faultMatrix.pngLimitEnforced and
  .faultMatrix.collisionFailAndRename and .faultMatrix.twoLaneRenameDistinct and
  .faultMatrix.staleOffsetRejected and
  .faultMatrix.oversizedChunkRejected and .faultMatrix.badDigestCleaned and
  .faultMatrix.cancelCleanedAndReleased and
  (($bytes | tonumber) <= 1073741824 or
    (.crossBridgeCleanupExercised and .crossBridgeCleanupSafe))
' "$evidence/result.json" >/dev/null
[[ "$max_connections" == 3 ]]
[[ "$(phase8_stat_size "$manager_download")" == "$transfer_bytes" ]]

final_path="$(jq -r .uploadFinalPath "$evidence/result.json")"
ssh -F "$evidence/ssh-config" "$target" \
  "stat -c 'path=%n mode=%a uid=%u size=%s blocks=%b' \"\$HOME/.cache/tmux-agent-ide/uploads\" '$final_path'; test \"\$(stat -c %a \"\$HOME/.cache/tmux-agent-ide/uploads\")\" = 700; test \"\$(stat -c %a '$final_path')\" = 600; test \"\$(stat -c %s '$final_path')\" = '$transfer_bytes'; test -z \"\$(find \"\$HOME/.cache/tmux-agent-ide/uploads\" -maxdepth 1 -type f -name '.tmux-agent-upload-*.partial' -print -quit)\"; truncate -s 0 '$final_path'" \
  >"$evidence/staging-stat.txt"
manager_upload_path="$(jq -r .uploadFinalPath "$evidence/result-manager.json")"
ssh -F "$evidence/ssh-config" "$target" "truncate -s 0 '$manager_upload_path'"
truncate -s 0 "$manager_download"

# Run the parent-replacement security gate after the transfer evidence so a
# fail-closed defect remains independently diagnosable in result-security.json.
ADE_PHASE7_STAGING_DIR=/home/ade/.cache/tmux-agent-ide/uploads \
  "$driver_binary" parent-swap ssh "$evidence/ssh-config" "$target" \
  >"$evidence/result-security.json" 2>"$evidence/security-driver.log" & security_pid=$!
if wait "$security_pid"; then security_status=0; else security_status=$?; fi
security_pid=''
printf '%s\n' "$security_status" >"$evidence/security-exit-status.txt"
jq -e '.commitFailedClosed and (.attackerDestinationPublished | not) and
  (.backupDestinationPublished | not) and (.partialLeakedAfterParentSwap | not) and
  .stagingRecovered and .parentSwapSafe' "$evidence/result-security.json" >/dev/null
[[ "$security_status" == 0 ]]
phase7_embed_source_digest "$evidence" "$evidence/result-security.json"
phase7_assert_source_tree_unchanged "$repo_root" "$evidence"
for artifact in result.json result-manager.json result-security.json source-tree-manifest.tsv \
  source-tree-digest.txt transfer-component-manifest.tsv transfer-component-digest.txt \
  driver.log manager-test.log security-driver.log tcp-samples.tsv \
  max-established-ssh-connections.txt staging-stat.txt security-exit-status.txt; do
  [[ ! -f "$evidence/$artifact" ]] || cp "$evidence/$artifact" "$durable_evidence/$artifact"
done
phase8_storage_publish "$repo_root" phase7-ssh-latest
if [[ "$transfer_bytes" == 5368709120 ]]; then
  phase8_storage_publish "$repo_root" phase7-ssh-5gib-latest
fi
printf '%s\n' "phase7 shaped Docker/OpenSSH evidence: $durable_evidence"
