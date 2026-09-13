#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase2 backend integration failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase2-backend-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/target}"
host_binary="$main_target/debug/muxflow-host"
driver_binary="$driver_target/debug/protocol-test-driver"
local_socket="ade-phase2-local-$$"
local_runtime="$runtime/local-runtime"
image_name="muxflow-phase2-ssh"
container_name="muxflow-phase2-$run_id"
ssh_key="$runtime/ssh-key"
ssh_config="$runtime/ssh-config"
known_hosts="$runtime/known-hosts"
remote_artifact="${ADE_TEST_BOOKWORM_HELPER:-$runtime/muxflow-host-bookworm}"
local_client_pid=""
remote_client_pid=""
local_client_name=""
remote_client_name=""
remote_client_pid_file="/tmp/muxflow-phase2-ordinary-$run_id.pid"

cleanup() {
  if [[ -n "$local_client_pid" ]]; then
    kill "$local_client_pid" >/dev/null 2>&1 || true
    wait "$local_client_pid" >/dev/null 2>&1 || true
  fi
  exec 9>&- 2>/dev/null || true
  [[ -z "${local_client_input:-}" ]] || rm -f "$local_client_input"
  if [[ -n "$remote_client_pid" ]]; then
    kill "$remote_client_pid" >/dev/null 2>&1 || true
    wait "$remote_client_pid" >/dev/null 2>&1 || true
  fi
  ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$local_socket" kill-server >/dev/null 2>&1 || true
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p \
  "$runtime" \
  "$local_runtime" \
  "$runtime/docker-config"
chmod 0700 "$runtime" "$local_runtime" "$runtime/docker-config"
# Keep buildx's activity metadata inside the disposable evidence directory;
# the managed QA sandbox intentionally exposes the user's Docker config read-only.
phase8_isolate_docker_config "$runtime/docker-config"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase2-backend-latest"

cd "$repo_root"
cargo build --bin muxflow-host >"$runtime/cargo-host-build.log" 2>&1 &
build_host_pid=$!
cargo build --manifest-path tests/integration/protocol/protocol-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1 &
build_driver_pid=$!
wait "$build_host_pid"
wait "$build_driver_pid"
"$host_binary" version >"$runtime/host-version.json"
tmux -V >"$runtime/local-tmux-version.txt"
docker info --format '{{.ServerVersion}}' >"$runtime/docker-version.txt"

snapshot_tmux_config() {
  local prefix="$1"
  shift
  "$@" show-options -g >"$prefix.global-options"
  "$@" show-options -s >"$prefix.server-options"
  "$@" show-window-options -g >"$prefix.global-window-options"
  "$@" list-keys >"$prefix.keys"
  "$@" show-options -gqv prefix >"$prefix.prefix"
  "$@" show-options -gqv status >"$prefix.status"
}

compare_tmux_config() {
  local before="$1"
  local after="$2"
  diff -u "$before.global-options" "$after.global-options"
  diff -u "$before.server-options" "$after.server-options"
  diff -u "$before.global-window-options" "$after.global-window-options"
  diff -u "$before.keys" "$after.keys"
  diff -u "$before.prefix" "$after.prefix"
  diff -u "$before.status" "$after.status"
}

wait_for_local_client() {
  for _ in $(seq 1 100); do
    local_client_name="$(tmux -L "$local_socket" list-clients 2>/dev/null | head -n 1 | sed 's/:.*//' || true)"
    [[ -n "$local_client_name" ]] && return 0
    sleep 0.02
  done
  return 1
}

tmux -L "$local_socket" new-session -d -s primary 'exec bash'
tmux -L "$local_socket" new-session -d -s external 'exec bash'
snapshot_tmux_config "$runtime/local.before" tmux -L "$local_socket"
# The ordinary control-mode client must keep stdin open for the whole run. A
# `tail -f /dev/null` feeder produces no output, so it never receives SIGPIPE
# when the client dies, and `wait` on a pipeline does not return until every
# member has exited — cleanup blocked forever after killing the client. Hold
# stdin open with a descriptor on a private FIFO so there is no second process.
local_client_input="$runtime/local-ordinary-client.in"
rm -f "$local_client_input"
mkfifo -m 0600 "$local_client_input"
# A fixed descriptor number, because macOS ships bash 3.2 and the {var}<>file
# form needs 4.1 or newer.
exec 9<>"$local_client_input"
tmux -L "$local_socket" -C attach-session -t primary <&9 \
  >/dev/null 2>"$runtime/local-ordinary-client.err" &
local_client_pid=$!
wait_for_local_client

"$driver_binary" matrix \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$local_runtime" \
  --tmux-socket "$local_socket" \
  --primary primary \
  --ordinary-client "$local_client_name" \
  >"$runtime/local-matrix.json"
jq -e '
  .transport == "local" and .protocolActions and .externalActions and
  .destructiveConfirmation and .ordinaryClient and .sessionSidecarOrder and
  .layoutConvergence and .exactOuterTopology and
  .exactOnceMembershipOutput and (.pipelinedInputLatencyMs < 2000) and
  (.controlLatencyMs < 2000) and .hiddenReleaseReattach and
  .backpressureResnapshot and .overflowSignalObserved and .seedModeDiagnostics
' "$runtime/local-matrix.json" >/dev/null

"$driver_binary" daemon-reconnect \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$local_runtime" \
  --tmux-socket "$local_socket" \
  --primary primary \
  >"$runtime/local-daemon-reconnect.json"
jq -e '.daemonReconnect and .stableSessionIdentity and .offlineOutputReseeded' \
  "$runtime/local-daemon-reconnect.json" >/dev/null

[[ "$(tmux -L "$local_socket" list-sessions -F '#{session_name}' | sort | tr '\n' ' ')" == "external primary " ]]
[[ "$(phase8_stat_mode "$local_runtime/session-order.json")" == "600" ]]
if tmux -L "$local_socket" show-options -Aqv @tmux_agent_ide_order | rg .; then
  echo 'local Phase 2 mutated a tmux user option' >&2
  exit 1
fi
kill -0 "$local_client_pid"
tmux -L "$local_socket" list-clients | sed 's/:.*//' | rg -Fx "$local_client_name" >/dev/null
snapshot_tmux_config "$runtime/local.after" tmux -L "$local_socket"
compare_tmux_config "$runtime/local.before" "$runtime/local.after"

# Build once per source digest against the Debian/glibc floor, then reuse the
# immutable helper across every SSH regression lane.
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_artifact" \
    >"$runtime/cargo-remote-build.log" 2>&1
fi
ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
docker build --platform "$docker_platform" -t "$image_name" "$repo_root/tests/integration/transport/ssh-target" \
  >"$runtime/docker-build.log" 2>&1 &
docker_build_pid=$!
wait "$docker_build_pid"

host_port=""
for candidate in $(seq 22322 22421); do
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
printf '%s\n' \
  'Host ade-phase2-docker' \
  '  HostName 127.0.0.1' \
  "  Port $host_port" \
  '  User ade' \
  "  IdentityFile $ssh_key" \
  '  IdentitiesOnly yes' \
  '  BatchMode yes' \
  '  ConnectTimeout 5' \
  '  ServerAliveInterval 1' \
  '  ServerAliveCountMax 2' \
  '  StrictHostKeyChecking accept-new' \
  "  UserKnownHostsFile $known_hosts" >"$ssh_config"
chmod 0600 "$ssh_config"
for _ in $(seq 1 100); do
  ssh -F "$ssh_config" ade-phase2-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$ssh_config" ade-phase2-docker true
docker exec "$container_name" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

remote_digest="$(sha256sum "$remote_artifact" | cut -d' ' -f1)"
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase2-docker --config "$ssh_config" \
  --artifact "$remote_artifact" --digest "$remote_digest" --expected-arch "$(uname -m)" \
  >"$runtime/remote-helper-install.log"
ssh -F "$ssh_config" ade-phase2-docker \
  '$HOME/.local/bin/muxflow-host version; tmux -V' >"$runtime/remote-versions.txt"
ssh -F "$ssh_config" ade-phase2-docker \
  '$HOME/.local/bin/muxflow-host daemon-stop' >/dev/null 2>&1 || true
ssh -F "$ssh_config" ade-phase2-docker \
  'tmux new-session -d -s primary "exec bash"; tmux new-session -d -s external "exec bash"'

remote_tmux() {
  local remote_command="tmux"
  local argument=""
  local quoted=""
  for argument in "$@"; do
    printf -v quoted ' %q' "$argument"
    remote_command+="$quoted"
  done
  ssh -F "$ssh_config" ade-phase2-docker "$remote_command"
}
snapshot_tmux_config "$runtime/remote.before" remote_tmux
ssh -F "$ssh_config" -T ade-phase2-docker \
  "rm -f '$remote_client_pid_file'; tail -f /dev/null | tmux -C attach-session -t primary & client_pid=\$!; printf '%s\\n' \"\$client_pid\" > '$remote_client_pid_file'; wait \"\$client_pid\"" \
  >/dev/null 2>"$runtime/remote-ordinary-client.err" &
remote_client_pid=$!
for _ in $(seq 1 100); do
  remote_client_process_pid="$(ssh -F "$ssh_config" ade-phase2-docker "cat '$remote_client_pid_file'" 2>/dev/null || true)"
  while IFS='|' read -r candidate_pid candidate_name; do
    if [[ -n "$remote_client_process_pid" && "$candidate_pid" == "$remote_client_process_pid" ]]; then
      remote_client_name="$candidate_name"
      break
    fi
  done < <(remote_tmux list-clients -F '#{client_pid}|#{client_name}' 2>/dev/null || true)
  [[ -n "$remote_client_name" ]] && break
  sleep 0.05
done
[[ -n "$remote_client_name" ]]

"$driver_binary" matrix \
  --transport ssh \
  --ssh-config "$ssh_config" \
  --ssh-target ade-phase2-docker \
  --primary primary \
  --ordinary-client "$remote_client_name" \
  >"$runtime/remote-matrix.json"
jq -e '
  .transport == "ssh" and .protocolActions and .externalActions and
  .destructiveConfirmation and .ordinaryClient and .sessionSidecarOrder and
  .layoutConvergence and .exactOuterTopology and
  .exactOnceMembershipOutput and (.pipelinedInputLatencyMs < 2000) and
  (.controlLatencyMs < 2000) and .hiddenReleaseReattach and
  .backpressureResnapshot and .overflowSignalObserved and .seedModeDiagnostics
' "$runtime/remote-matrix.json" >/dev/null

"$driver_binary" daemon-reconnect \
  --transport ssh \
  --ssh-config "$ssh_config" \
  --ssh-target ade-phase2-docker \
  --primary primary \
  >"$runtime/remote-daemon-reconnect.json"
jq -e '.daemonReconnect and .stableSessionIdentity and .offlineOutputReseeded' \
  "$runtime/remote-daemon-reconnect.json" >/dev/null

# An in-flight request fails on forced network loss. Output produced through
# docker exec while SSH is disconnected is present in the fresh terminal seed.
ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase2-docker \
  --config "$ssh_config" --terminal-session primary --interrupt-delay-ms 30000 \
  >"$runtime/remote-network-interrupt.json" 2>"$runtime/remote-network-interrupt.err" &
network_client_pid=$!
sleep 0.5
docker network disconnect bridge "$container_name"
if wait "$network_client_pid"; then
  echo 'Phase 2 in-flight request unexpectedly survived forced network loss' >&2
  exit 1
fi
docker exec --user ade "$container_name" \
  tmux send-keys -t primary "printf 'PHASE1_OFFLINE_OUTPUT\\n'" Enter
docker network connect bridge "$container_name"
for _ in $(seq 1 100); do
  ssh -F "$ssh_config" ade-phase2-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase2-docker \
  --config "$ssh_config" --terminal-session primary \
  >"$runtime/remote-network-recovered.json"
jq -e '.transport == "ssh" and .sessions == 2 and .offlineOutputReseeded and .overflowDetected' \
  "$runtime/remote-network-recovered.json" >/dev/null

[[ "$(remote_tmux list-sessions -F '#{session_name}' | sort | tr '\n' ' ')" == "external primary " ]]
[[ "$(ssh -F "$ssh_config" ade-phase2-docker "stat -c '%a' \"\$HOME/.local/state/muxflow/session-order.json\"")" == "600" ]]
if remote_tmux show-options -Aqv @tmux_agent_ide_order | rg .; then
  echo 'remote Phase 2 mutated a tmux user option' >&2
  exit 1
fi
# The deliberate Docker network disconnect above terminates this SSH-backed
# ordinary client; coexistence was asserted before that forced transport loss.
snapshot_tmux_config "$runtime/remote.after" remote_tmux
compare_tmux_config "$runtime/remote.before" "$runtime/remote.after"

printf '%s\n' \
  'phase2-backend: pass' \
  "artifacts: $runtime" \
  'routes: local, Docker SSH (100 ms netem)' \
  'scope: deterministic backend/protocol only; no CUA/frontend/manual claim'
