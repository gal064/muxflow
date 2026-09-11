#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase1 integration failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
# The deliberate-mismatch cases below need an architecture the target is not,
# so derive it rather than hardcoding one that stops being wrong when the gate
# runs natively on Apple Silicon.
case "$target_arch" in
  x86_64) mismatch_arch=aarch64 ;;
  *) mismatch_arch=x86_64 ;;
esac
runtime="$repo_root/tmp/phase1-linux"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
host_binary="$main_target/debug/muxflow-host"
remote_host_binary="${ADE_TEST_BOOKWORM_HELPER:-$runtime/muxflow-host-bookworm}"
rollback_artifact="$runtime/muxflow-host-rollback-fixture"
local_socket_name="ade-phase1-local-$$"
local_runtime="$runtime/local-runtime-$$"
local_socket="$local_runtime/host.sock"
container_name="ade-phase1-ssh"
image_name="ade-phase1-ssh:local"
ssh_key="$runtime/id_ed25519"
ssh_config="$runtime/ssh_config"
known_hosts="$runtime/known_hosts"
control_socket="$runtime/control.sock"
local_daemon_pid=""

wait_local_input_count() {
  local expected="$1"
  for _ in $(seq 1 100); do
    # BSD wc pads its count with leading spaces, so compare numerically rather
    # than as text; a string compare never matches on macOS.
    if [[ -f /tmp/muxflow-phase1-input-count ]] &&
      (($(wc -c </tmp/muxflow-phase1-input-count) == expected)); then
      return 0
    fi
    sleep 0.02
  done
  return 1
}

wait_remote_input_count() {
  local expected="$1"
  for _ in $(seq 1 100); do
    if [[ "$(ssh -F "$ssh_config" ade-phase1-docker 'wc -c < /tmp/muxflow-phase1-input-count' 2>/dev/null || true)" == "$expected" ]]; then
      return 0
    fi
    sleep 0.05
  done
  return 1
}

cleanup() {
  if [[ -n "$local_daemon_pid" ]]; then
    kill "$local_daemon_pid" >/dev/null 2>&1 || true
    wait "$local_daemon_pid" >/dev/null 2>&1 || true
  fi
  tmux -L "$local_socket_name" kill-server >/dev/null 2>&1 || true
  ssh -F "$ssh_config" -S "$control_socket" -O exit ade-phase1-docker >/dev/null 2>&1 || true
  docker rm -f "$container_name" >/dev/null 2>&1 || true
  rm -f /tmp/muxflow-phase1-input-count
}
trap cleanup EXIT

mkdir -p "$runtime"
chmod 0700 "$runtime"
cargo build --bin muxflow-host
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_host_binary"
fi

rm -f /tmp/muxflow-phase1-input-count
tmux -L "$local_socket_name" new-session -d -s phase1 'bash'
ADE_HOST_RUNTIME_DIR="$local_runtime" \
ADE_TMUX_SOCKET_NAME="$local_socket_name" \
ADE_PHASE1_TESTING=1 \
  "$host_binary" daemon --socket "$local_socket" >"$runtime/local-daemon.log" 2>&1 &
local_daemon_pid=$!
for _ in $(seq 1 100); do
  [[ -S "$local_socket" ]] && break
  sleep 0.02
done
[[ -S "$local_socket" ]]
[[ "$(phase8_stat_mode "$local_runtime")" == "700" ]]
[[ "$(phase8_stat_mode "$local_socket")" == "600" ]]
[[ "$(phase8_stat_uid "$local_runtime")" == "$(id -u)" ]]
[[ "$(phase8_stat_uid "$local_socket")" == "$(id -u)" ]]
if phase8_pid_tcp_listeners "$local_daemon_pid" >/dev/null; then
  echo 'host daemon unexpectedly opened a TCP listener' >&2
  exit 1
fi

ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket_name" ADE_PHASE1_TESTING=1 \
  "$host_binary" phase1-client local >"$runtime/local-smoke.json"
jq -e '
  .transport == "local" and .readOnly == false and .sessions == 1 and
  .sequenceGapDetected == true and .overflowDetected == true and
  .cancellation == "pass" and .scopedSnapshot == true and .terminalInputRouted == true
' "$runtime/local-smoke.json" >/dev/null
wait_local_input_count 1

ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket_name" \
  "$host_binary" phase1-client local --watch-panes 2 >"$runtime/local-watch.json" &
watch_pid=$!
sleep 0.25
tmux -L "$local_socket_name" split-window -h -t phase1
wait "$watch_pid"
jq -e '.panes == 1 and (.eventCounts.TopologySnapshot // 0) >= 1' "$runtime/local-watch.json" >/dev/null

ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket_name" \
  "$host_binary" phase1-client local --protocol-major 99 >"$runtime/protocol-incompatible.json"
jq -e '.readOnly == true' "$runtime/protocol-incompatible.json" >/dev/null
ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket_name" \
  "$host_binary" phase1-client local --expected-helper 99.0.0 >"$runtime/helper-incompatible.json"
jq -e '.readOnly == true' "$runtime/helper-incompatible.json" >/dev/null

kill "$local_daemon_pid"
wait "$local_daemon_pid" >/dev/null 2>&1 || true
local_daemon_pid=""
ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket_name" ADE_PHASE1_TESTING=1 \
  "$host_binary" phase1-client local >"$runtime/local-daemon-recovered.json"
jq -e '.sessions == 1 and .panes == 2 and .terminalInputRouted == true' "$runtime/local-daemon-recovered.json" >/dev/null
wait_local_input_count 2

if [[ ! -f "$ssh_key" ]]; then
  ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
fi
docker build --platform "$docker_platform" -t "$image_name" "$repo_root/tests/integration/transport/ssh-target"
docker rm -f "$container_name" >/dev/null 2>&1 || true
host_port=""
for candidate in $(seq 22322 22421); do
  if ! phase8_tcp_port_listening "$candidate"; then
    host_port="$candidate"
    break
  fi
done
[[ -n "$host_port" ]]
if [[ -f "$known_hosts" ]]; then
  ssh-keygen -q -R "[127.0.0.1]:$host_port" -f "$known_hosts" >/dev/null
fi
docker run -d --platform "$docker_platform" --name "$container_name" --cap-add NET_ADMIN \
  -p "127.0.0.1:$host_port:22" \
  -v "$ssh_key.pub:/config/authorized_keys:ro" "$image_name" >/dev/null
printf '%s\n' \
  'Host ade-phase1-docker' \
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
  ssh -F "$ssh_config" ade-phase1-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$ssh_config" ade-phase1-docker true
docker exec "$container_name" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit
docker exec "$container_name" mv /usr/bin/git /usr/bin/git.phase1-disabled

ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper probe ade-phase1-docker --config "$ssh_config" >"$runtime/helper-absent.json"
jq -e '.operatingSystem == "Linux" and .gitVersion == "unavailable" and .installed == false' \
  "$runtime/helper-absent.json" >/dev/null

digest="$(sha256sum "$remote_host_binary" | cut -d' ' -f1)"
if ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$(printf '0%.0s' $(seq 1 64))" --expected-arch "$target_arch"; then
  echo 'digest mismatch unexpectedly replaced remote helper' >&2
  exit 1
fi
if ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$mismatch_arch"; then
  echo 'architecture mismatch unexpectedly replaced remote helper' >&2
  exit 1
fi
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$target_arch"
docker exec "$container_name" mv /usr/bin/git.phase1-disabled /usr/bin/git
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper probe ade-phase1-docker --config "$ssh_config" >"$runtime/helper-installed.json"
jq -e --arg digest "$digest" '.installed == true and .digest == $digest' "$runtime/helper-installed.json" >/dev/null
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$target_arch" >"$runtime/helper-current.log"
rg 'helper-current: pass' "$runtime/helper-current.log" >/dev/null

installed_digest="$(ssh -F "$ssh_config" ade-phase1-docker 'sha256sum "$HOME/.local/bin/muxflow-host" | cut -d" " -f1')"
if ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$(printf '0%.0s' $(seq 1 64))" \
  --expected-arch "$target_arch" --allow-upgrade; then
  echo 'digest mismatch unexpectedly replaced installed remote helper' >&2
  exit 1
fi
[[ "$(ssh -F "$ssh_config" ade-phase1-docker 'sha256sum "$HOME/.local/bin/muxflow-host" | cut -d" " -f1')" == "$installed_digest" ]]
if ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$mismatch_arch" \
  --allow-upgrade; then
  echo 'architecture mismatch unexpectedly replaced installed remote helper' >&2
  exit 1
fi
[[ "$(ssh -F "$ssh_config" ade-phase1-docker 'sha256sum "$HOME/.local/bin/muxflow-host" | cut -d" " -f1')" == "$installed_digest" ]]

ssh -F "$ssh_config" ade-phase1-docker \
  'cp "$HOME/.local/bin/muxflow-host" "$HOME/.local/bin/muxflow-host.broken"; printf x >> "$HOME/.local/bin/muxflow-host.broken"; mv -f "$HOME/.local/bin/muxflow-host.broken" "$HOME/.local/bin/muxflow-host"'
if ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$target_arch"; then
  echo 'upgrade without explicit approval unexpectedly succeeded' >&2
  exit 1
fi
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$target_arch" --allow-upgrade

# An older/incompatible daemon cannot acknowledge the current cooperative
# shutdown operation. Its private metadata must be verified before fallback
# termination, and the replacement must still pass a fresh handshake.
ssh -F "$ssh_config" ade-phase1-docker \
  '"$HOME/.local/bin/muxflow-host" daemon-stop >/dev/null 2>&1 || true; runtime=/tmp/muxflow-$(id -u); for i in $(seq 1 100); do [ ! -S "$runtime/host.sock" ] && break; sleep 0.05; done; [ ! -S "$runtime/host.sock" ]; ADE_PHASE1_TESTING=1 ADE_PHASE1_TEST_PROTOCOL_MAJOR=99 nohup "$HOME/.local/bin/muxflow-host" daemon </dev/null >/dev/null 2>&1 &'
for _ in $(seq 1 100); do
  phase8_timeout 5 5 ssh -F "$ssh_config" ade-phase1-docker \
    'test -S /tmp/muxflow-1000/host.sock' && break
  sleep 0.05
done
ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$remote_host_binary" --digest "$digest" --expected-arch "$target_arch"
ssh -F "$ssh_config" ade-phase1-docker \
  '"$HOME/.local/bin/muxflow-host" protocol-check >/dev/null'

# A failure after atomic replacement and daemon shutdown restores the prior
# artifact and starts a healthy daemon. A failed first install leaves no final
# executable behind.
cp "$remote_host_binary" "$rollback_artifact"
printf '\0' >>"$rollback_artifact"
rollback_digest="$(sha256sum "$rollback_artifact" | cut -d' ' -f1)"
if ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --artifact "$rollback_artifact" --digest "$rollback_digest" --expected-arch "$target_arch" \
  --allow-upgrade --test-fail-after-shutdown; then
  echo 'post-replacement fault unexpectedly completed' >&2
  exit 1
fi
[[ "$(ssh -F "$ssh_config" ade-phase1-docker 'sha256sum "$HOME/.local/bin/muxflow-host" | cut -d" " -f1')" == "$digest" ]]
ssh -F "$ssh_config" ade-phase1-docker \
  '"$HOME/.local/bin/muxflow-host" protocol-check >/dev/null'
if ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
  "$host_binary" helper install ade-phase1-docker --config "$ssh_config" \
  --remote-path '$HOME/.local/bin/muxflow-host-first-fail' \
  --artifact "$rollback_artifact" --digest "$rollback_digest" --expected-arch "$target_arch" \
  --allow-upgrade --test-fail-after-shutdown; then
  echo 'failed first-install fault unexpectedly completed' >&2
  exit 1
fi
ssh -F "$ssh_config" ade-phase1-docker \
  'test ! -e "$HOME/.local/bin/muxflow-host-first-fail"; nohup "$HOME/.local/bin/muxflow-host" daemon </dev/null >/dev/null 2>&1 &'
for _ in $(seq 1 100); do
  phase8_timeout 5 5 ssh -F "$ssh_config" ade-phase1-docker \
    '"$HOME/.local/bin/muxflow-host" protocol-check >/dev/null' && break
  sleep 0.05
done

# The installer intentionally starts a production daemon to verify the new
# binary. Restart it through the testing bridge for the fault-injection cases.
ssh -F "$ssh_config" ade-phase1-docker \
  "pkill -f '/home/ade/.local/bin/muxflow-host daemon' || true" || true

ssh -F "$ssh_config" ade-phase1-docker \
  'rm -f /tmp/muxflow-phase1-input-count; tmux new-session -d -s phase1 "bash"'
ssh -F "$ssh_config" -M -N -f -o ControlMaster=yes -o ControlPersist=60 \
  -S "$control_socket" ade-phase1-docker
ssh -F "$ssh_config" -T -o ControlMaster=no -o ControlPath=none ade-phase1-docker \
  'head -c 67108864 /dev/zero' >/dev/null &
bulk_pid=$!
ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase1-docker \
  --config "$ssh_config" --control-socket "$control_socket" >"$runtime/remote-smoke.json"
wait "$bulk_pid"
jq -e '
  .transport == "ssh" and .readOnly == false and .sessions == 1 and
  .sequenceGapDetected == true and .overflowDetected == true and
  .cancellation == "pass" and .scopedSnapshot == true and .terminalInputRouted == true
' "$runtime/remote-smoke.json" >/dev/null
wait_remote_input_count 1
[[ "$(ssh -F "$ssh_config" ade-phase1-docker "stat -c '%a' /tmp/muxflow-1000")" == "700" ]]
[[ "$(ssh -F "$ssh_config" ade-phase1-docker "stat -c '%a' /tmp/muxflow-1000/host.sock")" == "600" ]]
[[ "$(ssh -F "$ssh_config" ade-phase1-docker "stat -c '%u' /tmp/muxflow-1000")" == "1000" ]]
[[ "$(ssh -F "$ssh_config" ade-phase1-docker "stat -c '%u' /tmp/muxflow-1000/host.sock")" == "1000" ]]

ssh -F "$ssh_config" ade-phase1-docker \
  "pkill -f '/home/ade/.local/bin/muxflow-host daemon' || true" || true
ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase1-docker \
  --config "$ssh_config" --control-socket "$control_socket" >"$runtime/remote-daemon-recovered.json"
jq -e '.sessions == 1 and .terminalInputRouted == true' "$runtime/remote-daemon-recovered.json" >/dev/null
wait_remote_input_count 2

ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase1-docker \
  --config "$ssh_config" --control-socket "$control_socket" --interrupt-delay-ms 30000 \
  >"$runtime/remote-network-interrupt.json" 2>"$runtime/remote-network-interrupt.err" &
remote_interrupt_pid=$!
sleep 0.3
docker network disconnect bridge "$container_name"
if wait "$remote_interrupt_pid"; then
  echo 'outstanding request unexpectedly survived forced network loss' >&2
  exit 1
fi
docker exec --user ade "$container_name" \
  tmux send-keys -t phase1 "printf 'PHASE1_OFFLINE_OUTPUT\\n'" Enter
docker network connect bridge "$container_name"
for _ in $(seq 1 100); do
  ssh -F "$ssh_config" ade-phase1-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ADE_PHASE1_TESTING=1 "$host_binary" phase1-client ssh ade-phase1-docker \
  --config "$ssh_config" >"$runtime/remote-network-recovered.json"
jq -e '.sessions == 1 and .offlineOutputReseeded == true and .terminalInputRouted == true' \
  "$runtime/remote-network-recovered.json" >/dev/null
wait_remote_input_count 3

echo 'phase1-linux-integration: pass'
