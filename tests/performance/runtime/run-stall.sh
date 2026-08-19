#!/usr/bin/env bash
# Phase 12 reconnect budget: "reconnect after 2 s network stall -> no disconnect".
#
# A disposable Linux/OpenSSH container is stalled by dropping every packet on
# its interface for the stall window, then unblocked. A live subscription is
# held across the stall; the budget is met only when that same connection is
# still answering afterwards with the same topology.
set -euo pipefail
trap 'echo "phase12 stall probe failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase12-stall-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/tmux-ide-host"
driver_binary="$driver_target/release/performance-test-driver"
image_name="tmux-agent-ide-phase12-ssh"
container_name="tmux-agent-ide-phase12-stall-$run_id"
ssh_key="$runtime/ssh-key"
ssh_config="$runtime/ssh-config"
known_hosts="$runtime/known-hosts"
remote_artifact="${ADE_TEST_BOOKWORM_HELPER:-$runtime/tmux-ide-host-bookworm}"
stall_seconds="${ADE_PHASE12_STALL_SECONDS:-2}"

cleanup() {
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$runtime" "$runtime/docker-config"
chmod 0700 "$runtime" "$runtime/docker-config"
phase8_isolate_docker_config "$runtime/docker-config"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase12-stall-latest"

cd "$repo_root"
cargo build --release --bin tmux-ide-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_artifact" \
    >"$runtime/cargo-remote-build.log" 2>&1
fi
ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
docker build --platform "$docker_platform" -t "$image_name" \
  "$repo_root/tests/integration/transport/ssh-target" >"$runtime/docker-build.log" 2>&1

host_port=""
for candidate in $(seq 22622 22721); do
  if ! phase8_tcp_port_listening "$candidate"; then
    host_port="$candidate"
    break
  fi
done
[[ -n "$host_port" ]]
docker run -d --platform "$docker_platform" --name "$container_name" --cap-add NET_ADMIN \
  -p "127.0.0.1:$host_port:22" \
  -v "$ssh_key.pub:/config/authorized_keys:ro" "$image_name" \
  >"$runtime/docker-container-id"

# The stall probe measures the app's own keepalive policy, so the client config
# here carries only connectivity settings; keepalive comes from the transport.
printf '%s\n' \
  'Host ade-phase12-stall' \
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
  ssh -F "$ssh_config" ade-phase12-stall true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$ssh_config" ade-phase12-stall true

remote_digest="$(sha256sum "$remote_artifact" | cut -d' ' -f1)"
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase12-stall --config "$ssh_config" \
  --artifact "$remote_artifact" --digest "$remote_digest" --expected-arch "$(uname -m)" \
  >"$runtime/remote-helper-install.log"
ssh -F "$ssh_config" ade-phase12-stall 'tmux new-session -d -s primary "exec bash"'

"$driver_binary" stall \
  --transport ssh \
  --ssh-config "$ssh_config" \
  --ssh-target ade-phase12-stall \
  --primary primary \
  --label "stall-${stall_seconds}s" \
  --stall-seconds "$stall_seconds" \
  --stall-script "docker exec $container_name tc qdisc add dev eth0 root netem loss 100%" \
  --restore-script "docker exec $container_name tc qdisc del dev eth0 root" \
  >"$runtime/stall.json"

cat "$runtime/stall.json"
jq -e '.connectionSurvivedStall and .answeredAfterStall and .topologyPreserved' \
  "$runtime/stall.json" >/dev/null || {
  echo "FAIL  docker  reconnect after ${stall_seconds}s stall  disconnected  no disconnect" >&2
  exit 1
}
printf 'PASS  docker  reconnect after %ss stall  survived  no disconnect\n' "$stall_seconds"
printf 'artifacts: %s\n' "$runtime"
