#!/usr/bin/env bash
# Phase 12 performance harness (plan stage 12.8).
#
# Every number produced here is machine-measured by tests/performance/runtime/perf-driver
# against a live host over the real transport. Nothing is derived from
# perception or from a screenshot. Two lanes run by default:
#
#   A  local  — this machine's tmux over the local daemon bridge
#   B  docker — disposable Linux/OpenSSH target shaped to 100 ms RTT
#
# Each lane also runs the raw `tmux -C` comparator over the identical link, so
# the app's added overhead beyond raw ssh+tmux is a measured subtraction rather
# than an assertion.
#
# Environment:
#   ADE_PHASE12_SKIP_DOCKER=1   run lane A only (records lane B as skipped)
#   ADE_PHASE12_ENFORCE=1       exit non-zero when a budget row fails
#   ADE_PHASE12_FLOOD_SECONDS=n flood probe duration (default 10 for exploratory
#                               runs and 60 when ADE_PHASE12_ENFORCE=1)
#   ADE_PHASE12_LABEL=<name>    label written into every result file
#   ADE_PHASE12_RUNTIME_FILE=p  write this invocation's exact artifact path to
#                               p, in addition to the compatibility latest file
set -euo pipefail
trap 'echo "phase12 perf harness failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
source "$repo_root/tests/integration/transfers/source-tree-evidence.sh"
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
label="${ADE_PHASE12_LABEL:-$run_id}"
runtime="$repo_root/tmp/phase12-perf-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/tmux-ide-host"
driver_binary="$driver_target/release/performance-test-driver"
if [[ -n "${ADE_PHASE12_FLOOD_SECONDS:-}" ]]; then
  flood_seconds="$ADE_PHASE12_FLOOD_SECONDS"
elif [[ "${ADE_PHASE12_ENFORCE:-0}" == "1" ]]; then
  flood_seconds=60
else
  flood_seconds=10
fi
if [[ "${ADE_PHASE12_ENFORCE:-0}" == "1" ]] && (( flood_seconds < 60 )); then
  echo 'phase12-perf: enforced evidence requires at least a 60 second flood' >&2
  exit 1
fi
local_socket="ade-phase12-perf-$$"
local_runtime="$runtime/local-runtime"
image_name="tmux-agent-ide-phase12-ssh"
container_name="tmux-agent-ide-phase12-$run_id"
ssh_key="$runtime/ssh-key"
ssh_config="$runtime/ssh-config"
known_hosts="$runtime/known-hosts"
remote_artifact="${ADE_TEST_BOOKWORM_HELPER:-$runtime/tmux-ide-host-bookworm}"
# The harness must reconcile stale topology on exactly the budget the shipped
# desktop uses, or its create-action numbers would describe a different product.
reconcile_timeout_ms="$(sed -n 's/^const ACTION_RECONCILE_TIMEOUT_MS = \([0-9_]*\);.*/\1/p' \
  "$repo_root/apps/desktop/src/features/tmux/actionReconciliation.ts" | tr -d '_')"
[[ -n "$reconcile_timeout_ms" ]]

cleanup() {
  ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$local_socket" kill-server >/dev/null 2>&1 || true
  docker rm -f "$container_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$runtime" "$local_runtime" "$runtime/docker-config"
chmod 0700 "$runtime" "$local_runtime" "$runtime/docker-config"
phase8_isolate_docker_config "$runtime/docker-config"
git rev-parse HEAD >"$runtime/source-commit.txt"
git status --porcelain >"$runtime/source-status.txt"
if [[ "${ADE_PHASE12_ENFORCE:-0}" == "1" ]] && [[ -s "$runtime/source-status.txt" ]]; then
  echo 'phase12-perf: enforced evidence requires a clean source tree' >&2
  exit 1
fi
phase7_capture_source_tree "$repo_root" "$runtime"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase12-perf-latest"
if [[ -n "${ADE_PHASE12_RUNTIME_FILE:-}" ]]; then
  printf '%s\n' "$runtime" >"$ADE_PHASE12_RUNTIME_FILE"
fi

cd "$repo_root"
# Release builds on both sides: an opt-level-0 parser is not the product.
cargo build --release --bin tmux-ide-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1
"$host_binary" version >"$runtime/host-version.json"
tmux -V >"$runtime/local-tmux-version.txt"

# ---------------------------------------------------------------------------
# Lane A — local
# ---------------------------------------------------------------------------
tmux -L "$local_socket" new-session -d -s primary 'exec bash'
tmux -L "$local_socket" new-session -d -s external 'exec bash'
local_pane="$(tmux -L "$local_socket" list-panes -t primary -F '#{pane_id}' | head -n 1)"

"$driver_binary" raw-tmux \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$local_runtime" \
  --tmux-socket "$local_socket" \
  --primary primary \
  --pane "$local_pane" \
  --label "$label-local-raw" \
  >"$runtime/local-raw.json"

"$driver_binary" perf \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$local_runtime" \
  --tmux-socket "$local_socket" \
  --primary primary \
  --reconcile-timeout-ms "$reconcile_timeout_ms" \
  --flood-seconds "$flood_seconds" \
  --label "$label-local-app" \
  >"$runtime/local-app.json"

ADE_HOST_RUNTIME_DIR="$local_runtime" ADE_TMUX_SOCKET_NAME="$local_socket" \
  "$host_binary" daemon-stop >/dev/null 2>&1 || true
tmux -L "$local_socket" kill-server >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Lane B — Docker SSH shaped to 100 ms
# ---------------------------------------------------------------------------
docker_status="ran"
if [[ "${ADE_PHASE12_SKIP_DOCKER:-0}" == "1" ]]; then
  docker_status="skipped-by-request"
elif ! docker info --format '{{.ServerVersion}}' >"$runtime/docker-version.txt" 2>"$runtime/docker-unavailable.txt"; then
  docker_status="blocked-docker-unavailable"
fi

if [[ "$docker_status" == "ran" ]]; then
  if [[ -z "${ADE_TEST_BOOKWORM_HELPER:-}" ]]; then
    release/linux/build-compatible-host.sh "$target_arch" "$remote_artifact" \
      >"$runtime/cargo-remote-build.log" 2>&1
  fi
  ssh-keygen -q -t ed25519 -N '' -f "$ssh_key"
  docker build --platform "$docker_platform" -t "$image_name" \
    "$repo_root/tests/integration/transport/ssh-target" >"$runtime/docker-build.log" 2>&1

  host_port=""
  for candidate in $(seq 22522 22621); do
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
  docker cp "$repo_root/tests/performance/runtime/tmux-counting-wrapper.sh" \
    "$container_name:/usr/local/bin/tmux" >/dev/null
  docker exec "$container_name" chmod 0755 /usr/local/bin/tmux
  printf '%s\n' \
    'Host ade-phase12-docker' \
    '  HostName 127.0.0.1' \
    "  Port $host_port" \
    '  User ade' \
    "  IdentityFile $ssh_key" \
    '  IdentitiesOnly yes' \
    '  BatchMode yes' \
    '  ConnectTimeout 5' \
    '  ServerAliveInterval 15' \
    '  ServerAliveCountMax 3' \
    '  StrictHostKeyChecking accept-new' \
    "  UserKnownHostsFile $known_hosts" >"$ssh_config"
  chmod 0600 "$ssh_config"
  for _ in $(seq 1 100); do
    ssh -F "$ssh_config" ade-phase12-docker true >/dev/null 2>&1 && break
    sleep 0.05
  done
  ssh -F "$ssh_config" ade-phase12-docker true
  docker exec "$container_name" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

  remote_digest="$(sha256sum "$remote_artifact" | cut -d' ' -f1)"
  ADE_HOST_RUNTIME_DIR="$runtime/installer-runtime" \
    "$host_binary" helper install ade-phase12-docker --config "$ssh_config" \
    --artifact "$remote_artifact" --digest "$remote_digest" --expected-arch "$(uname -m)" \
    >"$runtime/remote-helper-install.log"
  ssh -F "$ssh_config" ade-phase12-docker \
    '$HOME/.local/bin/tmux-ide-host version; tmux -V' >"$runtime/remote-versions.txt"
  ssh -F "$ssh_config" ade-phase12-docker \
    'tmux new-session -d -s primary "exec bash"; tmux new-session -d -s external "exec bash"'
  remote_pane="$(ssh -F "$ssh_config" ade-phase12-docker \
    "tmux list-panes -t primary -F '#{pane_id}'" | head -n 1 | tr -d '\r')"

  "$driver_binary" raw-tmux \
    --transport ssh \
    --ssh-config "$ssh_config" \
    --ssh-target ade-phase12-docker \
    --primary primary \
    --pane "$remote_pane" \
    --label "$label-docker-raw" \
    >"$runtime/docker-raw.json"

  "$driver_binary" perf \
    --transport ssh \
    --ssh-config "$ssh_config" \
    --ssh-target ade-phase12-docker \
    --primary primary \
    --reconcile-timeout-ms "$reconcile_timeout_ms" \
    --flood-seconds "$flood_seconds" \
    --label "$label-docker-app" \
    >"$runtime/docker-app.json"
  docker cp "$container_name:/tmp/ade-phase12-tmux-process.log" \
    "$runtime/docker-tmux-process.log" >/dev/null
  uv run --no-project "$repo_root/tests/performance/runtime/summarize-tmux-processes.py" \
    "$runtime/docker-tmux-process.log" >"$runtime/docker-tmux-process.json"
fi

{
  sha256sum "$host_binary"
  sha256sum "$driver_binary"
  if [[ -f "$remote_artifact" ]]; then
    sha256sum "$remote_artifact"
  fi
} >"$runtime/binary-digests.txt"
phase7_assert_source_tree_unchanged "$repo_root" "$runtime"

# ---------------------------------------------------------------------------
# Budget table
# ---------------------------------------------------------------------------
printf '%s\n' "$docker_status" >"$runtime/docker-status.txt"
bash "$repo_root/tests/performance/runtime/budget-report.sh" "$runtime" >"$runtime/budget-report.txt"
cat "$runtime/budget-report.txt"

if [[ "${ADE_PHASE12_ENFORCE:-0}" == "1" ]] \
  && grep -Eq '^(FAIL|BLOCK)' "$runtime/budget-report.txt"; then
  echo 'phase12-perf: budget rows failed' >&2
  exit 1
fi

printf '%s\n' \
  'phase12-perf: complete' \
  "artifacts: $runtime" \
  "lanes: local, docker=$docker_status"
