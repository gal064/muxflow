#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase6 Docker SSH gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase6-ssh-$run_id"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/agents/protocol-driver/target}"
driver_binary="$driver_target/debug/agent-test-driver"
remote_helper="${ADE_TEST_BOOKWORM_HELPER:-$runtime/muxflow-host-bookworm}"
container="ade-phase6-$run_id"
image="muxflow-phase6-ssh"
cleanup() {
  if [[ "${ADE_PHASE6_KEEP_CONTAINER:-0}" != "1" ]]; then
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
mkdir -p "$runtime/docker-config"
chmod 0700 "$runtime" "$runtime/docker-config"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase6-ssh-latest"
phase8_isolate_docker_config "$runtime/docker-config"
cd "$repo_root"

cargo build --manifest-path tests/integration/agents/protocol-driver/Cargo.toml >"$runtime/driver-build.log" 2>&1 & driver_pid=$!
docker build --platform "$docker_platform" -t "$image" tests/integration/transport/ssh-target >"$runtime/docker-build.log" 2>&1 & image_pid=$!
if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
  release/linux/build-compatible-host.sh "$target_arch" "$remote_helper" \
    >"$runtime/remote-build.log" 2>&1 & remote_pid=$!
else
  remote_pid=''
fi
docker run --rm --platform "$docker_platform" --user "$(id -u):$(id -g)" -v "$repo_root:/workspace" -w /workspace rust:1.97.1-slim-bookworm \
  rustc --edition 2024 tests/integration/agents/agent-fixture.rs -o "/workspace/${runtime#"$repo_root/"}/agent-fixture" \
  >"$runtime/fixture-build.log" 2>&1 & fixture_pid=$!
wait "$driver_pid"
wait "$image_pid"
[[ -z "$remote_pid" ]] || wait "$remote_pid"
wait "$fixture_pid"

ssh-keygen -q -t ed25519 -N '' -f "$runtime/ssh-key"
port=""
for candidate in $(seq 22622 22721); do
  if ! phase8_tcp_port_listening "$candidate"; then port="$candidate"; break; fi
done
[[ -n "$port" ]]
docker run -d --platform "$docker_platform" --name "$container" --cap-add NET_ADMIN -p "127.0.0.1:$port:22" -v "$runtime/ssh-key.pub:/config/authorized_keys:ro" "$image" >"$runtime/container-id"
printf '%s\n' 'Host ade-phase6-docker' '  HostName 127.0.0.1' "  Port $port" '  User ade' \
  "  IdentityFile $runtime/ssh-key" '  IdentitiesOnly yes' '  BatchMode yes' '  ConnectTimeout 5' \
  '  StrictHostKeyChecking accept-new' "  UserKnownHostsFile $runtime/known-hosts" >"$runtime/ssh-config"
chmod 0600 "$runtime/ssh-config"
for _ in $(seq 1 100); do ssh -F "$runtime/ssh-config" ade-phase6-docker true >/dev/null 2>&1 && break; sleep 0.05; done
docker exec "$container" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

scp -q -F "$runtime/ssh-config" "$remote_helper" ade-phase6-docker:/home/ade/muxflow-host
scp -q -F "$runtime/ssh-config" "$runtime/agent-fixture" ade-phase6-docker:/home/ade/agent-fixture
docker cp "$runtime/agent-fixture" "$container:/usr/local/bin/codex"
docker cp "$runtime/agent-fixture" "$container:/usr/local/bin/claude"
docker exec "$container" chmod 0755 /usr/local/bin/codex /usr/local/bin/claude
ssh -F "$runtime/ssh-config" ade-phase6-docker '
  set -eu
  install -d -m 0700 "$HOME/.local/bin" "$HOME/phase6-bin" "$HOME/phase6-home/.codex" "$HOME/phase6-home/.claude" "$HOME/phase6-runtime" "$HOME/phase6-repo"
  install -m 0700 "$HOME/muxflow-host" "$HOME/.local/bin/muxflow-host"
  install -m 0700 "$HOME/agent-fixture" "$HOME/phase6-bin/codex"
  install -m 0700 "$HOME/agent-fixture" "$HOME/phase6-bin/claude"
  printf "%s\n" "{\"unrelated\":{\"private\":\"phase6-private-value\"},\"hooks\":{\"UnrelatedEvent\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"preserve-codex\"}]}]}}" >"$HOME/phase6-home/.codex/hooks.json"
  printf "%s\n" "{\"unrelated\":{\"private\":\"phase6-private-value\"},\"hooks\":{\"UnrelatedEvent\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"preserve-claude\"}]}]}}" >"$HOME/phase6-home/.claude/settings.json"
  printf "%s\n" phase6 >"$HOME/phase6-repo/README.md"
  git -C "$HOME/phase6-repo" init -q
  tmux -L ade-phase6 new-session -d -s phase6 -c "$HOME/phase6-repo" "exec bash"
  tmux -L ade-phase6 set-environment -g PATH "$HOME/phase6-bin:/usr/local/bin:/usr/bin:/bin"
'

"$driver_binary" ssh "$runtime/ssh-config" ade-phase6-docker >"$runtime/result.json"
jq -e '.privateUnixSocket and .adapterRegistry and .manualDetection and .launchWindow and .launchSplit and .activeRootInherited and .workingBlockedDone and .duplicateRejected and .outOfOrderRejected and .seenExact and .exactDirectRoute and .foreignUnmapped and (.hooks.codex.staleRejected) and (.hooks["claude-code"].staleRejected)' "$runtime/result.json" >/dev/null

# Prove the actual remote hook executable delivers only to the remote daemon's
# private Unix socket. The protocol driver above deliberately exercises the
# stdio bridge separately, so start a daemon and invoke the CLI as a hook would.
ssh -F "$runtime/ssh-config" ade-phase6-docker '
  env HOME="$HOME/phase6-home" ADE_HOST_RUNTIME_DIR="$HOME/phase6-runtime" ADE_TMUX_SOCKET_NAME=ade-phase6 "$HOME/.local/bin/muxflow-host" daemon >"$HOME/phase6-runtime/daemon.log" 2>&1 </dev/null &
'
for _ in $(seq 1 50); do
  if ssh -F "$runtime/ssh-config" ade-phase6-docker 'test -S "$HOME/phase6-runtime/host.sock"'; then break; fi
  sleep 0.1
done
ssh -F "$runtime/ssh-config" ade-phase6-docker '
  pane=$(tmux -L ade-phase6 list-panes -a -f "#{==:#{pane_current_command},bash}" -F "#{pane_id}" | head -n 1)
  test -n "$pane"
  printf "%s\n" "$pane" > "$HOME/phase6-runtime/actual-hook-pane.txt"
  command="printf %s eyJob29rX2V2ZW50X25hbWUiOiJVc2VyUHJvbXB0U3VibWl0Iiwic2Vzc2lvbl9pZCI6InJlbW90ZS1jbGktcHJvb2YifQ== | base64 -d | env HOME=\"$HOME/phase6-home\" ADE_HOST_RUNTIME_DIR=\"$HOME/phase6-runtime\" \"$HOME/.local/bin/muxflow-host\" hook ingest --adapter codex"
  printf "%s" "$command" | tmux -L ade-phase6 load-buffer -
  tmux -L ade-phase6 paste-buffer -d -t "$pane"
  tmux -L ade-phase6 send-keys -t "$pane" Enter
  for _ in $(seq 1 50); do test -s "$HOME/phase6-runtime/agents.json" && break; sleep 0.1; done
  test -s "$HOME/phase6-runtime/agents.json"
'
remote_hook_pane=$(ssh -F "$runtime/ssh-config" ade-phase6-docker \
  'cat "$HOME/phase6-runtime/actual-hook-pane.txt"')
ssh -F "$runtime/ssh-config" ade-phase6-docker \
  'cat "$HOME/phase6-runtime/agents.json"' >"$runtime/actual-hook-agents.json"
jq -e --arg pane "$remote_hook_pane" '[.agents[] | select(
  .native_session_id == "remote-cli-proof" and
  .route.pane_id == $pane and
  .route.session_id != "" and
  .route.window_id != ""
)] | length == 1' "$runtime/actual-hook-agents.json" >/dev/null

# The helper exposes only its mode-0700 Unix socket. SSH is the sole TCP listener
# added by this disposable target; no hook or daemon port is public.
ssh -F "$runtime/ssh-config" ade-phase6-docker 'ss -H -ltnp; stat -c "%a %U %F" "$HOME/phase6-runtime/host.sock"' >"$runtime/listeners.txt"
if ssh -F "$runtime/ssh-config" ade-phase6-docker 'ss -H -ltn | awk "{print \$4}" | grep -Ev "(:22|127.0.0.11:)$"' >"$runtime/unexpected-listeners.txt"; then
  echo "unexpected public listener" >&2; cat "$runtime/unexpected-listeners.txt" >&2; exit 1
fi

# Daemon-down delivery writes one bounded, privacy-minimized fallback envelope.
ssh -F "$runtime/ssh-config" ade-phase6-docker '
  set -eu
  env HOME="$HOME/phase6-home" ADE_HOST_RUNTIME_DIR="$HOME/phase6-runtime" ADE_TMUX_SOCKET_NAME=ade-phase6 "$HOME/.local/bin/muxflow-host" daemon-stop
  for _ in $(seq 1 50); do
    test ! -S "$HOME/phase6-runtime/host.sock" && break
    sleep 0.1
  done
  test ! -S "$HOME/phase6-runtime/host.sock"
  printf "%s" "{\"hook_event_name\":\"Stop\",\"session_id\":\"offline\",\"prompt\":\"do-not-store\",\"api_token\":\"secret\"}" | env HOME="$HOME/phase6-home" ADE_HOST_RUNTIME_DIR="$HOME/phase6-runtime" ADE_TMUX_SOCKET_NAME=ade-phase6 TMUX_PANE=%777 "$HOME/.local/bin/muxflow-host" hook ingest --adapter codex
  test -f "$HOME/phase6-runtime/hook-fallback-codex-777.pb"
  ! grep -aE "do-not-store|secret" "$HOME/phase6-runtime/hook-fallback-codex-777.pb"
'

ssh -F "$runtime/ssh-config" ade-phase6-docker '
  grep -q preserve-codex "$HOME/phase6-home/.codex/hooks.json"
  grep -q preserve-claude "$HOME/phase6-home/.claude/settings.json"
  test -f "$HOME/phase6-home/.codex/hooks.json.muxflow.backup"
  test -f "$HOME/phase6-home/.claude/settings.json.muxflow.backup"
  ! grep -q muxflow "$HOME/phase6-home/.codex/hooks.json"
  ! grep -q muxflow "$HOME/phase6-home/.claude/settings.json"
' >"$runtime/config-preservation.txt"

echo "phase6 Docker SSH evidence: $runtime"
