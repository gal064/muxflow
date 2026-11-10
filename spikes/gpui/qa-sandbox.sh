#!/usr/bin/env bash
# Sandbox for QA that launches the app or helper. It makes the owner's session
# unreachable, instead of relying on each script to unset the right variables.
#
#   qa-sandbox.sh up <name>             # start; state lives in $QA_SANDBOX_ROOT/<name>
#   qa-sandbox.sh run <name> <cmd...>   # run a command inside (state persists between runs)
#   qa-sandbox.sh down <name>           # stop everything inside and remove the state
#
# Inside: the filesystem is read-only except $STATE and private /tmp, so no owner
# file can change. /tmp and /run/user are private, which hides the tmux, helper,
# Wayland and D-Bus sockets. A private network namespace hides the abstract X11
# socket. A private PID namespace means no owner process can be signalled.
# ~/.config, ~/.local, ~/.cache and ~/.ssh are empty, and HOME is $STATE/home.
#
# Docker stays reachable through its socket. There is no network, so reach an SSH
# target with this in $STATE/home/.ssh/config:
#   ProxyCommand docker exec -i <container> socat - TCP:localhost:22
# ssh finds ~/.ssh through passwd, not $HOME, so pass that config with -F (the
# app has an SSH config field).
# Connecting to the Docker socket can still affect any container: use only
# containers named for your task.
set -euo pipefail

root="${QA_SANDBOX_ROOT:-${TMPDIR:-/tmp}/muxflow-qa-sandbox}"
action="${1:?up, run or down}"
name="${2:?sandbox name}"
shift 2
state="$root/$name"

entered_env=(env -i "HOME=$state/home" "PATH=$PATH" "USER=${USER:-qa}" "LANG=C.UTF-8"
  "XDG_RUNTIME_DIR=$state/runtime" "ADE_HOST_RUNTIME_DIR=/tmp/host-runtime"
  "QA_SANDBOX_STATE=$state")

child_pid() {
  sed -n 's/.*"child-pid": *\([0-9]*\).*/\1/p' "$state/info"
}

case "$action" in
  up)
    [[ ! -e "$state" ]] || { echo "already exists: $state" >&2; exit 1; }
    mkdir -p "$state/home/.ssh" "$state/runtime" "$state/etc-ssh"
    chmod 700 "$state/home/.ssh" "$state/runtime"
    # ssh rejects a config owned by the unmapped root, so it gets its own copy.
    cp -rL /etc/ssh/ssh_config /etc/ssh/ssh_config.d "$state/etc-ssh/" 2>/dev/null || true

    args=(--ro-bind / / --dev /dev --proc /proc --tmpfs /tmp --perms 0700 --dir /tmp/host-runtime --tmpfs "/run/user/$(id -u)"
      --bind "$state" "$state" --ro-bind "$state/etc-ssh" /etc/ssh
      --unshare-user --unshare-net --unshare-pid --unshare-ipc --unshare-uts)
    for hidden in .config .local .cache .ssh; do
      [[ -d "$HOME/$hidden" ]] && args+=(--tmpfs "$HOME/$hidden")
    done
    nohup bwrap "${args[@]}" --info-fd 3 sleep infinity 3>"$state/info" >/dev/null 2>&1 &
    echo $! >"$state/bwrap.pid"
    for _ in $(seq 50); do [[ -n "$(child_pid)" ]] && break; sleep 0.1; done
    [[ -n "$(child_pid)" ]] || { echo "sandbox did not start" >&2; exit 1; }
    echo "$state"
    ;;
  run)
    exec nsenter -t "$(child_pid)" -U -m -n -p -i -u --preserve-credentials \
      "--wd=$PWD" "${entered_env[@]}" "$@"
    ;;
  down)
    # The child is PID 1 of the sandbox, so killing it ends everything inside.
    kill -KILL "$(child_pid)" 2>/dev/null || true
    kill "$(cat "$state/bwrap.pid")" 2>/dev/null || true
    rm -rf -- "$state"
    ;;
  *)
    echo "unknown action: $action" >&2
    exit 2
    ;;
esac
