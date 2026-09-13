#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase5 Docker SSH gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase5-ssh-$run_id"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/target}"
driver="$driver_target/debug/git-test-driver"
remote_helper="${ADE_TEST_BOOKWORM_HELPER:-$runtime/muxflow-host-bookworm}"
container="ade-phase5-$run_id"
image="muxflow-phase5-ssh"
cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required for Phase 5 SSH evidence" >&2
  exit 2
fi
mkdir -p "$runtime/docker-config"
chmod 0700 "$runtime" "$runtime/docker-config"
phase8_isolate_docker_config "$runtime/docker-config"
cd "$repo_root"
cargo build --manifest-path tests/integration/git/protocol-driver/Cargo.toml >"$runtime/driver-build.log" 2>&1 &
driver_pid=$!
docker build --platform "$docker_platform" -t "$image" tests/integration/transport/ssh-target >"$runtime/docker-build.log" 2>&1 &
image_pid=$!
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_helper" \
    >"$runtime/remote-build.log" 2>&1 &
  remote_pid=$!
else
  remote_pid=''
fi
wait "$driver_pid"
wait "$image_pid"
[[ -z "$remote_pid" ]] || wait "$remote_pid"

ssh-keygen -q -t ed25519 -N '' -f "$runtime/ssh-key"
port=""
for candidate in $(seq 22522 22621); do
  if ! phase8_tcp_port_listening "$candidate"; then port="$candidate"; break; fi
done
[[ -n "$port" ]]
docker run -d --platform "$docker_platform" --name "$container" --cap-add NET_ADMIN -p "127.0.0.1:$port:22" \
  -v "$runtime/ssh-key.pub:/config/authorized_keys:ro" "$image" >"$runtime/container-id"
printf '%s\n' 'Host ade-phase5-docker' '  HostName 127.0.0.1' "  Port $port" '  User ade' \
  "  IdentityFile $runtime/ssh-key" '  IdentitiesOnly yes' '  BatchMode yes' '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' "  UserKnownHostsFile $runtime/known-hosts" >"$runtime/ssh-config"
chmod 0600 "$runtime/ssh-config"
for _ in $(seq 1 100); do
  ssh -F "$runtime/ssh-config" ade-phase5-docker true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$runtime/ssh-config" ade-phase5-docker 'mkdir -p "$HOME/.local/bin" "$HOME/phase5-repo"'
scp -q -F "$runtime/ssh-config" "$remote_helper" ade-phase5-docker:/home/ade/.local/bin/muxflow-host
ssh -F "$runtime/ssh-config" ade-phase5-docker \
  'chmod 0700 "$HOME/.local/bin/muxflow-host"; repo="$HOME/phase5-repo"; git -C "$repo" init -q; git -C "$repo" config user.name "Phase Five"; git -C "$repo" config user.email phase5@example.test; printf "%s\n" "ignored*" >"$repo/.gitignore"; printf "%s\n" base >"$repo/tracked"; git -C "$repo" add .gitignore tracked; git -C "$repo" commit -qm base; printf "%s\n" changed >"$repo/tracked"; printf "%s\n" ignored >"$repo/ignored-one"; printf "%s\n" literal >"$repo/:(glob)*"; printf "%s\n" ordinary >"$repo/ordinary"; raw="$(printf "raw-\377")"; printf "%s\n" raw >"$repo/$raw"; printf "%s\n" "#!/bin/sh" "echo phase5-hook-blocked >&2" "exit 17" >"$repo/.git/hooks/pre-commit"; chmod 0700 "$repo/.git/hooks/pre-commit"; tmux new-session -d -s phase5 -c "$repo" "exec bash"'
docker exec "$container" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit
"$driver" ssh "$runtime/ssh-config" ade-phase5-docker \
  >"$runtime/remote.json" 2>"$runtime/remote-driver.log"
jq -e '.authoritativeStatus and .repositoryStableAcrossReconnect and .rawPathSafe and .ignored and .stage and .stagedDiff and .hookErrorSurfaced and .discardTokenEnforced and .pathEscapeRejected and .staleConnectionRejected and .literalPathspecSafe and .transportLossCancelledHook' "$runtime/remote.json" >/dev/null
printf '%s\n' "phase5 Docker SSH evidence: $runtime"
