#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
runtime_dir="$repo_root/tmp/phase0-docker-ssh"
container_name="ade-phase0-ssh"
image_name="ade-phase0-ssh:local"
ssh_key="$runtime_dir/id_ed25519"
ssh_config="$runtime_dir/ssh_config"
known_hosts="$runtime_dir/known_hosts"

remove_container() {
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
cleanup() {
  if [[ "${ADE_PHASE0_KEEP_CONTAINER:-0}" != "1" ]]; then
    remove_container
  fi
}
trap cleanup EXIT

mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"
if [[ ! -f "$ssh_key" ]]; then
  ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
fi

docker build --platform "$docker_platform" -t "$image_name" "$repo_root/tests/integration/transport/ssh-target"
remove_container

host_port=''
for candidate in $(seq 22222 22321); do
  if ! phase8_tcp_port_listening "$candidate"; then
    host_port="$candidate"
    break
  fi
done
if [[ -z "$host_port" ]]; then
  echo 'could not find a free localhost port for the SSH fixture' >&2
  exit 1
fi

# The container deliberately gets a fresh host key on every clean start. Drop
# only this loopback fixture's previous key so repeatable local QA neither
# disables host verification nor fails on an expected disposable identity.
if [[ -f "$known_hosts" ]]; then
  ssh-keygen -q -R "[127.0.0.1]:$host_port" -f "$known_hosts" >/dev/null
fi

docker run -d --platform "$docker_platform" \
  --name "$container_name" \
  --cap-add NET_ADMIN \
  -p "127.0.0.1:$host_port:22" \
  -v "$ssh_key.pub:/config/authorized_keys:ro" \
  "$image_name" >/dev/null
printf '%s\n' \
  'Host ade-phase0-docker' \
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
  "  UserKnownHostsFile $known_hosts" \
  > "$ssh_config"
chmod 0600 "$ssh_config"

for _ in $(seq 1 50); do
  if ssh -F "$ssh_config" ade-phase0-docker true 2>/dev/null; then
    break
  fi
  sleep 0.1
done
ssh -F "$ssh_config" ade-phase0-docker true

# Delay container egress by 100 ms, which makes request/reply latency at least 100 ms.
docker exec "$container_name" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

ADE_PHASE0_SSH_CONFIG="$ssh_config" \
  cargo run --quiet --bin tmux-ide-host -- phase0-ssh ade-phase0-docker

ssh -F "$ssh_config" ade-phase0-docker 'sleep 30' >/dev/null 2>&1 &
long_ssh_pid=$!
sleep 0.2
docker restart "$container_name" >/dev/null
if wait "$long_ssh_pid"; then
  echo 'expected the remote restart to terminate the active SSH session' >&2
  exit 1
fi

reconnected=0
for _ in $(seq 1 50); do
  if ssh -F "$ssh_config" ade-phase0-docker true 2>"$runtime_dir/reconnect-error.log"; then
    reconnected=1
    break
  fi
  sleep 0.1
done

if [[ "$reconnected" == "1" ]]; then
  echo 'remote-container-reconnect: pass'
  if [[ "${ADE_PHASE0_KEEP_CONTAINER:-0}" == "1" ]]; then
    ssh -F "$ssh_config" ade-phase0-docker \
      "tmux new-session -d -s phase0 'bash'; tmux split-window -h -t phase0 'bash'; tmux send-keys -t phase0:0.0 'printf \\\"REMOTE_PANE_LEFT\\\\n\\\"' Enter; tmux send-keys -t phase0:0.1 'printf \\\"REMOTE_PANE_RIGHT\\\\n\\\"' Enter"
    echo "persistent-fixture: pass config=$ssh_config host=ade-phase0-docker session=phase0"
  fi
  exit 0
fi

echo 'SSH did not recover after the remote container restart' >&2
docker ps -a --filter "name=^/${container_name}$" --format 'container={{.Status}} ports={{.Ports}}' >&2
docker logs "$container_name" >&2 || true
sed -n '1,80p' "$runtime_dir/reconnect-error.log" >&2 || true
exit 1
