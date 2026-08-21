#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../../.." && pwd -P)
[[ $(uname -s) == Darwin ]] || { echo "Mac-to-Docker gate requires macOS" >&2; exit 69; }
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
work="$repo/tmp/work/phase10/docker-$run_id"
container="ade-phase10-$run_id"
image='ade-phase10-ssh:local'
release_target=${CARGO_TARGET_DIR:-"$repo/tmp/work/cache/release-target/macos"}
app="$release_target/release/bundle/macos/Muxflow.app"
local_host="$app/Contents/MacOS/muxflow-host"
artifact="$app/Contents/Resources/muxflow-host-linux-aarch64"
key="$work/id_ed25519"
config="$work/ssh_config"
known_hosts="$work/known_hosts"

cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  [[ "$work" == "$repo/tmp/work/phase10/docker-"* ]] && rm -rf "$work"
}
trap cleanup EXIT

[[ -x "$local_host" && -x "$artifact" ]]
file "$local_host" | rg 'Mach-O.*arm64' >/dev/null
file "$artifact" | rg 'ELF.*ARM aarch64' >/dev/null
mkdir -p "$work"
chmod 0700 "$work"
ssh-keygen -q -t ed25519 -N '' -f "$key"
docker build --platform linux/arm64 -t "$image" "$repo/tests/integration/transport/ssh-target" >/dev/null
docker run -d --platform linux/arm64 --name "$container" --cap-add NET_ADMIN \
  -p 127.0.0.1::22 -v "$key.pub:/config/authorized_keys:ro" "$image" >/dev/null
port=$(docker port "$container" 22/tcp | sed -n 's/.*://p' | head -n 1)
[[ "$port" =~ ^[0-9]+$ ]]
printf '%s\n' \
  'Host ade-phase10-docker' \
  '  HostName 127.0.0.1' \
  "  Port $port" \
  '  User ade' \
  "  IdentityFile $key" \
  '  IdentitiesOnly yes' \
  '  BatchMode yes' \
  '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' \
  "  UserKnownHostsFile $known_hosts" >"$config"
chmod 0600 "$config"
for _ in $(seq 1 100); do
  ssh -F "$config" ade-phase10-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$config" ade-phase10-docker true
docker exec "$container" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

legacy="$work/legacy-helper"
cp "$artifact" "$legacy"
printf '\n' >>"$legacy"
chmod 0755 "$legacy"
legacy_digest=$(shasum -a 256 "$legacy" | cut -d' ' -f1)
artifact_digest=$(shasum -a 256 "$artifact" | cut -d' ' -f1)
"$local_host" helper install ade-phase10-docker --config "$config" \
  --artifact "$legacy" --digest "$legacy_digest" --expected-arch aarch64 >/dev/null
ssh -F "$config" ade-phase10-docker '$HOME/.local/bin/muxflow-host bridge --stdio </dev/null >/dev/null 2>&1 || true'
if ADE_PHASE1_TESTING=1 "$local_host" helper install ade-phase10-docker --config "$config" \
  --artifact "$artifact" --digest "$artifact_digest" --expected-arch aarch64 \
  --allow-upgrade --test-fail-after-shutdown >/dev/null 2>&1; then
  echo "injected helper upgrade unexpectedly succeeded" >&2
  exit 1
fi
remote_digest=$(ssh -F "$config" ade-phase10-docker 'sha256sum $HOME/.local/bin/muxflow-host' | cut -d' ' -f1)
[[ "$remote_digest" == "$legacy_digest" ]]
"$local_host" helper install ade-phase10-docker --config "$config" \
  --artifact "$artifact" --digest "$artifact_digest" --expected-arch aarch64 \
  --allow-upgrade >/dev/null
"$local_host" helper probe ade-phase10-docker --config "$config" \
  | jq -e '.operatingSystem == "Linux" and .architecture == "aarch64" and .compatible' >/dev/null

ssh -F "$config" ade-phase10-docker \
  'set -eu; tmux new-session -d -s ade-phase10-docker; tmux rename-window -t ade-phase10-docker:0 qa; mkdir -p $HOME/ade-phase10-repo; git -C $HOME/ade-phase10-repo init -q; printf "phase10\n" >$HOME/ade-phase10-repo/file; git -C $HOME/ade-phase10-repo add file; tmux kill-server; rm -rf $HOME/ade-phase10-repo $HOME/.local/bin/muxflow-host'
ssh -F "$config" ade-phase10-docker \
  'test ! -e $HOME/ade-phase10-repo && test ! -e $HOME/.local/bin/muxflow-host && ! tmux list-sessions >/dev/null 2>&1'

echo "PHASE10_DOCKER_SSH_SMOKE_PASS latency_ms=100 helper=linux-aarch64"
