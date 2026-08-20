#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
source "$repo_root/tests/release/storage.sh"
cd "$repo_root"
run_root="$repo_root/tmp/phase8-cua-$(date -u +%Y%m%dT%H%M%SZ)-$$"
home="$run_root/home"
prefix="$run_root/prefix"
runtime="$run_root/runtime"
tmux_runtime="$repo_root/tmp/phase8-tmux-$$"
config="$run_root/config"
data="$run_root/data"
cache="$run_root/cache"
local_repo="$run_root/local-repo"
container="ade-phase8-cua-$(date +%s)-$$"
tmux_socket="phase8-cua-$(date +%s)-$$"
mkdir -p "$home" "$runtime" "$tmux_runtime" "$config" "$data" "$cache" "$local_repo" "$run_root/evidence" "$run_root/docker-config"
chmod 0700 "$home" "$runtime" "$tmux_runtime" "$config" "$data" "$cache" "$run_root/docker-config"
ln -sfn "$(basename "$run_root")" "$repo_root/tmp/phase8-cua-latest"
handed_off=false
cleanup_failed_setup() {
  status=$?
  trap - EXIT
  if ! $handed_off; then
    pkill -f "$prefix/lib/muxflow/muxflow" >/dev/null 2>&1 || true
    HOME="$home" ADE_HOST_RUNTIME_DIR="$runtime" "$prefix/lib/muxflow/muxflow-host" daemon-stop >/dev/null 2>&1 || true
    env -u TMUX HOME="$home" XDG_RUNTIME_DIR="$runtime" TMUX_TMPDIR="$tmux_runtime" tmux -L "$tmux_socket" kill-server >/dev/null 2>&1 || true
    docker rm -f "$container" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup_failed_setup EXIT

archive=${ADE_PHASE8_CUA_ARCHIVE:-}
if [[ -z "$archive" ]]; then
  package_run=$(realpath "$repo_root/tmp/phase8-package-latest")
  archive=$(find "$package_run/output-a" -maxdepth 1 -name '*.tar.gz' -print -quit)
fi
release/linux/verify-package.sh "$archive" > "$run_root/package-verify.log"
mkdir -p "$run_root/unpack"
tar -xzf "$archive" -C "$run_root/unpack"
package_root=$(find "$run_root/unpack" -mindepth 1 -maxdepth 1 -type d -print -quit)
HOME="$home" ADE_INSTALL_PREFIX="$prefix" "$package_root/install.sh" > "$run_root/install.log"

git -C "$local_repo" init -q
git -C "$local_repo" config user.name 'Phase Eight CUA'
git -C "$local_repo" config user.email phase8@example.test
printf '# Phase 8\n\nInitial Markdown.\n' > "$local_repo/README.md"
printf 'baseline\n' > "$local_repo/tracked.txt"
printf 'ignored.log\n' > "$local_repo/.gitignore"
git -C "$local_repo" add .
git -C "$local_repo" commit -qm baseline
printf 'working tree change\n' >> "$local_repo/tracked.txt"
printf 'ignored\n' > "$local_repo/ignored.log"
printf 'paste file\n' > "$local_repo/local paste ü.txt"
cp tests/integration/agents/evidence/local-connected.png "$local_repo/clipboard.png"

phase8_isolate_docker_config "$run_root/docker-config"
docker build -t muxflow-phase8-cua tests/integration/transport/ssh-target > "$run_root/docker-build.log" 2>&1
ssh-keygen -q -t ed25519 -N '' -f "$run_root/ssh-key"
port=""
for candidate in $(seq 23122 23221); do
  if ! phase8_tcp_port_listening "$candidate"; then port="$candidate"; break; fi
done
[[ -n "$port" ]]
docker run -d --name "$container" --cap-add NET_ADMIN -p "127.0.0.1:$port:22" \
  -v "$run_root/ssh-key.pub:/config/authorized_keys:ro" muxflow-phase8-cua \
  > "$run_root/container-id"
printf '%s\n' 'Host phase8-cua-remote' '  HostName 127.0.0.1' "  Port $port" '  User ade' \
  "  IdentityFile $run_root/ssh-key" '  IdentitiesOnly yes' '  BatchMode yes' '  ConnectTimeout 5' \
  '  ServerAliveInterval 1' '  ServerAliveCountMax 2' '  StrictHostKeyChecking accept-new' \
  "  UserKnownHostsFile $run_root/known-hosts" > "$run_root/ssh-config"
chmod 0600 "$run_root/ssh-config"
for _ in $(seq 1 100); do
  ssh -F "$run_root/ssh-config" phase8-cua-remote true >/dev/null 2>&1 && break
  sleep 0.05
done
ssh -F "$run_root/ssh-config" phase8-cua-remote 'mkdir -p "$HOME/.local/bin" "$HOME/phase8-repo"'
scp -q -F "$run_root/ssh-config" "$prefix/lib/muxflow/muxflow-host-x86_64" phase8-cua-remote:/home/ade/.local/bin/muxflow-host
ssh -F "$run_root/ssh-config" phase8-cua-remote '
  chmod 0700 "$HOME/.local/bin/muxflow-host"
  repo="$HOME/phase8-repo"
  git -C "$repo" init -q
  git -C "$repo" config user.name "Phase Eight Remote"
  git -C "$repo" config user.email phase8@example.test
  printf "# Remote Phase 8\n" > "$repo/REMOTE.md"
  printf "remote baseline\n" > "$repo/remote.txt"
  git -C "$repo" add . && git -C "$repo" commit -qm baseline
  printf "remote change\n" >> "$repo/remote.txt"
  tmux new-session -d -s phase8-remote -c "$repo" "exec bash"
'
docker exec "$container" tc qdisc add dev eth0 root netem delay 100ms rate 100mbit

profile_dir="$config/dev.muxflow.desktop"
mkdir -p "$profile_dir"
printf '{\n  "schemaVersion": 1,\n  "profiles": [\n    {"id":"local","label":"Local","connection":{"mode":"local"}},\n    {"id":"phase8-remote","label":"Phase 8 Remote","connection":{"mode":"ssh","profileId":"phase8-remote","target":"phase8-cua-remote","configPath":"%s"}}\n  ],\n  "lastProfileId":"local"\n}\n' "$run_root/ssh-config" > "$profile_dir/profiles.json"
chmod 0600 "$profile_dir/profiles.json"

cat > "$run_root/launch.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export HOME='$home'
export XDG_CONFIG_HOME='$config'
export XDG_DATA_HOME='$data'
export XDG_CACHE_HOME='$cache'
export XDG_RUNTIME_DIR='$runtime'
export TMUX_TMPDIR='$tmux_runtime'
export ADE_TMUX_SOCKET_NAME='$tmux_socket'
unset TMUX
export ADE_HOST_HELPER_PATH='$prefix/lib/muxflow/muxflow-host'
if ! tmux -L '$tmux_socket' list-sessions >/dev/null 2>&1; then
  tmux -L '$tmux_socket' -f /dev/null new-session -d -s phase8-local -c '$local_repo' 'exec bash'
  tmux -L '$tmux_socket' -f /dev/null new-window -d -t phase8-local -n second -c '$local_repo' 'exec bash'
fi
exec '$prefix/lib/muxflow/muxflow' >> '$run_root/desktop.log' 2>&1
EOF
chmod 0700 "$run_root/launch.sh"
cat > "$run_root/start-scale-fixture.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export HOME='$home'
export XDG_RUNTIME_DIR='$runtime'
export TMUX_TMPDIR='$tmux_runtime'
unset TMUX
exec '$repo_root/tests/release/create-scale-fixture.sh' '$run_root/scale-repository' '$tmux_socket' >> '$run_root/scale-fixture.log' 2>&1
EOF
chmod 0700 "$run_root/start-scale-fixture.sh"
cat > "$run_root/mutate-local.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export HOME='$home'
export XDG_RUNTIME_DIR='$runtime'
export TMUX_TMPDIR='$tmux_runtime'
unset TMUX
tmux -L '$tmux_socket' split-window -h -t phase8-local:0 -c '$local_repo' 'exec bash'
printf '# External update\n' > '$local_repo/live-created.md'
printf '\nExternal Markdown update.\n' >> '$local_repo/README.md'
printf 'external git change\n' >> '$local_repo/tracked.txt'
EOF
chmod 0700 "$run_root/mutate-local.sh"
cat > "$run_root/flap-remote.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
docker pause '$container' >/dev/null
sleep 3
docker unpause '$container' >/dev/null
EOF
chmod 0700 "$run_root/flap-remote.sh"
printf '%s\n' "$container" > "$run_root/container-name"
printf '%s\n' "$tmux_socket" > "$run_root/tmux-socket-name"
printf '%s\n' "$tmux_runtime" > "$run_root/tmux-runtime"
handed_off=true
trap - EXIT
printf '%s\n' "$run_root"
