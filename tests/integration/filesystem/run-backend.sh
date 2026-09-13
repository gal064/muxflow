#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase4 backend integration failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase4-backend-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/target}"
host_binary="$main_target/debug/muxflow-host"
driver_binary="$driver_target/debug/filesystem-test-driver"
socket_name="ade-phase4-local-$$"
local_runtime="$runtime/local-runtime"
local_repo="$runtime/local-repo"
container_name="ade-phase4-$run_id"
image_name="muxflow-phase4-ssh"
ssh_key="$runtime/ssh-key"
ssh_config="$runtime/ssh-config"
known_hosts="$runtime/known-hosts"
remote_helper="${ADE_TEST_BOOKWORM_HELPER:-$runtime/muxflow-host-bookworm}"

cleanup() {
  ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$socket_name" kill-server >/dev/null 2>&1 || true
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$runtime" "$local_runtime" "$local_repo/subdir" "$runtime/docker-config"
chmod 0700 "$runtime" "$local_runtime" "$runtime/docker-config"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase4-backend-latest"

cd "$repo_root"
cargo build --bin muxflow-host >"$runtime/cargo-host.log" 2>&1 &
host_build_pid=$!
cargo build --manifest-path tests/integration/filesystem/protocol-driver/Cargo.toml \
  >"$runtime/cargo-driver.log" 2>&1 &
driver_build_pid=$!
wait "$host_build_pid"
wait "$driver_build_pid"

git -C "$local_repo" init -q
printf '%s\n' visible >"$local_repo/.dotfile"
yes x | head -c $((10 * 1024 * 1024)) >"$local_repo/editor-max.txt" || true
head -c $((25 * 1024 * 1024)) /dev/zero >"$local_repo/preview-25.png"
head -c $((25 * 1024 * 1024 + 1)) /dev/zero >"$local_repo/preview-over.png"
mkdir -p "$local_repo/.git/never-traverse" "$local_repo/node_modules/package"
tmux -L "$socket_name" new-session -d -s phase4 -c "$local_repo/subdir" 'exec bash'
ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
  "$driver_binary" local "$host_binary" >"$runtime/local.json"
jq -e '.activeRootAtomic and .rootToken and .dotfilesVisible and .gitCollapsed and .textRoundTrip and .blake3Verified and .folderDownload and .controlBodiesRejected and (.editorMaxReadWriteBytes == 10485760) and (.previewBoundaryBytes == 26214400) and .oversizedPreviewMetadataOnly and (.maxControlLatencyMs < 2500) and .nonEmptyConfirmation' \
  "$runtime/local.json" >/dev/null

phase8_isolate_docker_config "$runtime/docker-config"
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_helper" \
    >"$runtime/cargo-remote.log" 2>&1 &
  remote_build_pid=$!
else
  remote_build_pid=''
fi
docker build --platform "$docker_platform" -t "$image_name" "$repo_root/tests/integration/transport/ssh-target" \
  >"$runtime/docker-build.log" 2>&1 &
image_build_pid=$!
[[ -z "$remote_build_pid" ]] || wait "$remote_build_pid"
wait "$image_build_pid"

ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
host_port=""
for candidate in $(seq 22422 22521); do
  if ! phase8_tcp_port_listening "$candidate"; then
    host_port="$candidate"
    break
  fi
done
[[ -n "$host_port" ]]
docker run -d --platform "$docker_platform" --name "$container_name" --cap-add NET_ADMIN \
  -p "127.0.0.1:$host_port:22" \
  -v "$ssh_key.pub:/config/authorized_keys:ro" "$image_name" \
  >"$runtime/container-id"
printf '%s\n' \
  'Host ade-phase4-docker' \
  '  HostName 127.0.0.1' \
  "  Port $host_port" \
  '  User ade' \
  "  IdentityFile $ssh_key" \
  '  IdentitiesOnly yes' \
  '  BatchMode yes' \
  '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' \
  "  UserKnownHostsFile $known_hosts" >"$ssh_config"
chmod 0600 "$ssh_config"
for _ in $(seq 1 100); do
  ssh -F "$ssh_config" ade-phase4-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$ssh_config" ade-phase4-docker true
ssh -F "$ssh_config" ade-phase4-docker 'mkdir -p "$HOME/.local/bin" "$HOME/phase4-repo/subdir"'
scp -q -F "$ssh_config" "$remote_helper" \
  ade-phase4-docker:/home/ade/.local/bin/muxflow-host
ssh -F "$ssh_config" ade-phase4-docker \
  'chmod 0700 "$HOME/.local/bin/muxflow-host"; git -C "$HOME/phase4-repo" init -q; printf "%s\n" visible >"$HOME/phase4-repo/.dotfile"; yes x | head -c 10485760 >"$HOME/phase4-repo/editor-max.txt" || true; head -c 26214400 /dev/zero >"$HOME/phase4-repo/preview-25.png"; head -c 26214401 /dev/zero >"$HOME/phase4-repo/preview-over.png"; mkdir -p "$HOME/phase4-repo/.git/never-traverse" "$HOME/phase4-repo/node_modules/package"; tmux new-session -d -s phase4 -c "$HOME/phase4-repo/subdir" "exec bash"'
docker exec "$container_name" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit
"$driver_binary" ssh "$ssh_config" ade-phase4-docker >"$runtime/remote.json"
jq -e '.activeRootAtomic and .rootToken and .dotfilesVisible and .gitCollapsed and .textRoundTrip and .blake3Verified and .folderDownload and .controlBodiesRejected and (.editorMaxReadWriteBytes == 10485760) and (.previewBoundaryBytes == 26214400) and .oversizedPreviewMetadataOnly and (.maxControlLatencyMs < 2500) and .nonEmptyConfirmation' \
  "$runtime/remote.json" >/dev/null

printf '%s\n' "phase4 backend evidence: $runtime"
