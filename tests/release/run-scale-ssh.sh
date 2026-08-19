#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
phase8_storage_begin "$repo_root" phase8-scale-ssh
run_root=$PHASE8_WORK_DIR
evidence=$PHASE8_EVIDENCE_DIR
container="ade-phase8-scale-$(date +%s)-$$"
image="tmux-agent-ide-phase8-scale-ssh"
mkdir -p "$run_root/docker-config"
chmod 0700 "$run_root" "$run_root/docker-config"
phase8_isolate_docker_config "$run_root/docker-config"

cleanup() {
  local status=$?
  docker rm -f "$container" >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

cargo build --manifest-path tests/release/protocol-driver/Cargo.toml > "$run_root/driver-build.log" 2>&1 &
driver_pid=$!
docker build --platform "$docker_platform" -t "$image" tests/integration/transport/ssh-target > "$run_root/docker-build.log" 2>&1 &
image_pid=$!
wait "$driver_pid" "$image_pid"

# This lane needs exactly one thing from the release archive: the Linux helper
# binary it copies into the container. Building that archive requires a Linux
# host (it links a WebKitGTK desktop), so on macOS take the helper from
# release/linux/build-compatible-host.sh instead — the same reproducible builder
# every other gate validates, and now also the source of the helper shipped in
# the macOS package (M10-E047, M10-E048).
if phase8_is_darwin; then
  remote_helper="$run_root/tmux-ide-host-linux"
  release/linux/build-compatible-host.sh "$target_arch" "$remote_helper" \
    > "$run_root/package-verify.log"
else
  archive=${ADE_PHASE8_PACKAGE_ARCHIVE:-}
  if [[ -z "$archive" && -L "$repo_root/tmp/phase8-package-latest" ]]; then
    package_run=$(realpath "$repo_root/tmp/phase8-package-latest")
    archive=$(find "$package_run/output-a" -maxdepth 1 -name '*-linux-x86_64.tar.gz' -print -quit)
  fi
  if [[ -z "$archive" ]]; then
    ADE_RELEASE_OUTPUT_DIR="$run_root/release" release/linux/build-package.sh x86_64 > "$run_root/package.log"
    archive=$(tail -n 1 "$run_root/package.log")
  fi
  release/linux/verify-package.sh "$archive" > "$run_root/package-verify.log"
  mkdir -p "$run_root/unpack"
  tar -xzf "$archive" -C "$run_root/unpack"
  package_root=$(find "$run_root/unpack" -mindepth 1 -maxdepth 1 -type d -print -quit)
  remote_helper="$package_root/bin/tmux-ide-host"
fi

ssh-keygen -q -t ed25519 -N '' -f "$run_root/ssh-key"
port=""
for candidate in $(seq 22922 23021); do
  if ! phase8_tcp_port_listening "$candidate"; then port="$candidate"; break; fi
done
[[ -n "$port" ]]
docker run -d --platform "$docker_platform" --name "$container" --cap-add NET_ADMIN -p "127.0.0.1:$port:22" \
  -v "$run_root/ssh-key.pub:/config/authorized_keys:ro" "$image" > "$run_root/container-id"
printf '%s\n' 'Host ade-phase8-scale' '  HostName 127.0.0.1' "  Port $port" '  User ade' \
  "  IdentityFile $run_root/ssh-key" '  IdentitiesOnly yes' '  BatchMode yes' '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' "  UserKnownHostsFile $run_root/known-hosts" > "$run_root/ssh-config"
chmod 0600 "$run_root/ssh-config"
for _ in $(seq 1 100); do
  ssh -F "$run_root/ssh-config" ade-phase8-scale true >/dev/null 2>&1 && break
  sleep 0.05
done

scp -q -F "$run_root/ssh-config" "$remote_helper" ade-phase8-scale:/home/ade/tmux-ide-host
scp -q -F "$run_root/ssh-config" tests/release/create-scale-fixture.sh ade-phase8-scale:/home/ade/create-scale-fixture.sh
ssh -F "$run_root/ssh-config" ade-phase8-scale \
  'mkdir -p "$HOME/.local/bin" "$HOME/phase8-home" "$HOME/phase8-runtime"; mv "$HOME/tmux-ide-host" "$HOME/.local/bin/tmux-ide-host"; chmod 0700 "$HOME/.local/bin/tmux-ide-host" "$HOME/create-scale-fixture.sh"; "$HOME/create-scale-fixture.sh" "$HOME/phase8-repository"' \
  > "$run_root/fixture.log"
docker exec "$container" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

ADE_PHASE8_REMOTE_ENV='HOME=$HOME/phase8-home ADE_HOST_RUNTIME_DIR=$HOME/phase8-runtime' \
  "$CARGO_TARGET_DIR/debug/release-test-driver" ssh "$run_root/ssh-config" ade-phase8-scale \
  > "$run_root/result.json" 2> "$run_root/driver.log"
jq -e '.status == "pass" and .mode == "ssh-100ms" and .sessions >= 20 and .windows >= 100 and .rootEntries >= 250' \
  "$run_root/result.json" >/dev/null
for artifact in result.json fixture.log package-verify.log driver.log driver-build.log docker-build.log; do
  [[ ! -f "$run_root/$artifact" ]] || cp "$run_root/$artifact" "$evidence/$artifact"
done
phase8_storage_publish "$repo_root" phase8-scale-ssh-latest
cat "$run_root/result.json"
