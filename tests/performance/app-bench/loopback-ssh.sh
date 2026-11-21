#!/usr/bin/env bash
# An SSH host on this machine, without root, for the bench's SSH runs.
#
#   loopback-ssh.sh <dir> start [port]    # default port 22622
#   loopback-ssh.sh <dir> delay <ms>      # one-way delay for new connections
#   loopback-ssh.sh <dir> stop
#
# Starts a private sshd as the current user on 127.0.0.1, with its own host
# key and an authorized key it generates. Every session it serves runs in a
# separate account root under <dir>: its own HOME, TMUX_TMPDIR and
# ADE_HOST_RUNTIME_DIR, so the bench's remote tmux server, helper daemon,
# ~/.bashrc line and ~/.local/bin/muxflow-host never touch the user's own.
# (The helper still reads the passwd home for Claude's session files; it
# writes nothing there.)
#
# Writes <dir>/ssh_config with two hosts:
#   muxflow-bench          through workloads/delay-proxy.pl, which holds each
#                          chunk <dir>/delay_ms in each direction (RTT + 2x)
#   muxflow-bench-direct   straight to the sshd, for the harness's own commands
#
# Extra environment for remote sessions (for example MUXFLOW_CHAT_SCRIPTED=1)
# goes in <dir>/remote-env, one `export NAME=value` per line.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dir="${1:?usage: loopback-ssh.sh <dir> start|stop|delay}"
command="${2:?usage: loopback-ssh.sh <dir> start|stop|delay}"
mkdir -p "$dir"
dir="$(cd "$dir" && pwd)"
# The bench account's tmux socket name (ADE_TMUX_SOCKET_NAME): never "default".
socket_name=mxbench

case "$command" in
  stop)
    # Only what this script started, found through its own pid files and
    # checked before any signal: never a bare tmux, never a pattern kill.
    if [[ -s "$dir/sshd.pid" ]]; then
      pid="$(cat "$dir/sshd.pid")"
      if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF -- "-f $dir/etc/sshd_config"; then
        kill "$pid" 2>/dev/null || true
      fi
      rm -f "$dir/sshd.pid"
    fi
    # The bench account's tmux server, named by its full path: a bare
    # `tmux kill-server` run from inside a tmux pane follows $TMUX to the
    # user's own server, whatever TMUX_TMPDIR says.
    socket="$dir/t/tmux-$(id -u)/$socket_name"
    case "$socket" in
      "$dir"/t/*) ;;
      *) echo "refusing to touch tmux socket outside $dir: $socket" >&2; exit 70 ;;
    esac
    if [[ -S "$socket" ]]; then
      env -u TMUX -u TMUX_PANE tmux -S "$socket" kill-server 2>/dev/null || true
    fi
    # The bench account's helper daemon: its own runtime dir, so daemon-stop
    # cannot reach the user's; then its pid, if still alive and still ours.
    if [[ -x "$dir/home/.local/bin/muxflow-host" ]]; then
      env -u TMUX -u TMUX_PANE ADE_HOST_RUNTIME_DIR="$dir/r" \
        "$dir/home/.local/bin/muxflow-host" daemon-stop >/dev/null 2>&1 || true
    fi
    if [[ -s "$dir/r/daemon.json" ]]; then
      pid="$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$dir/r/daemon.json")"
      if [[ -n "$pid" ]] && tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null \
          | grep -qxF "ADE_HOST_RUNTIME_DIR=$dir/r"; then
        kill "$pid" 2>/dev/null || true
      fi
    fi
    exit 0
    ;;
  delay)
    echo "${3:?delay in ms}" > "$dir/delay_ms"
    exit 0
    ;;
  start) ;;
  *) echo "unknown command: $command" >&2; exit 64 ;;
esac

port="${3:-22622}"
mkdir -p "$dir/etc" "$dir/home" "$dir/t" "$dir/r"
chmod 700 "$dir/etc" "$dir/t" "$dir/r"
[[ -f "$dir/etc/host_ed25519" ]] || ssh-keygen -q -t ed25519 -N '' -C bench-host -f "$dir/etc/host_ed25519"
[[ -f "$dir/etc/client_ed25519" ]] || ssh-keygen -q -t ed25519 -N '' -C bench-client -f "$dir/etc/client_ed25519"
cp "$dir/etc/client_ed25519.pub" "$dir/etc/authorized_keys"
chmod 600 "$dir/etc/authorized_keys"
[[ -f "$dir/delay_ms" ]] || echo 0 > "$dir/delay_ms"
touch "$dir/remote-env"
# A login shell (a tmux pane's) reads ~/.bashrc, as on a usual account.
[[ -f "$dir/home/.bash_profile" ]] || echo '. ~/.bashrc' > "$dir/home/.bash_profile"
touch "$dir/home/.bashrc"

cat > "$dir/etc/session.sh" <<EOF
#!/bin/sh
# ForceCommand: every session runs in the bench account root, not the user's.
export HOME="$dir/home" SHELL=/bin/bash
export TMUX_TMPDIR="$dir/t" ADE_HOST_RUNTIME_DIR="$dir/r" ADE_TMUX_SOCKET_NAME=$socket_name
export XDG_CONFIG_HOME="$dir/home/.config" XDG_STATE_HOME="$dir/home/.local/state"
export XDG_DATA_HOME="$dir/home/.local/share" XDG_CACHE_HOME="$dir/home/.cache"
export PATH="$dir/home/.local/bin:/usr/local/bin:/usr/bin:/bin"
unset XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS TMUX TMUX_PANE
. "$dir/remote-env"
cd "\$HOME"
case "\${SSH_ORIGINAL_COMMAND:-}" in
  sftp|internal-sftp|*/sftp-server) exec /usr/lib/ssh/sftp-server ;;  # scp
esac
if [ -n "\${SSH_ORIGINAL_COMMAND:-}" ]; then
  exec /bin/bash -c "\$SSH_ORIGINAL_COMMAND"
fi
exec /bin/bash -l
EOF
chmod 755 "$dir/etc/session.sh"

cat > "$dir/etc/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/etc/host_ed25519
PidFile $dir/sshd.pid
AuthorizedKeysFile $dir/etc/authorized_keys
AllowUsers $(id -un)
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PermitUserEnvironment no
AllowTcpForwarding yes
X11Forwarding no
PrintMotd no
ForceCommand $dir/etc/session.sh
Subsystem sftp /usr/lib/ssh/sftp-server
EOF

cat > "$dir/ssh_config" <<EOF
Host muxflow-bench muxflow-bench-direct
  HostName 127.0.0.1
  Port $port
  User $(id -un)
  IdentityFile $dir/etc/client_ed25519
  IdentitiesOnly yes
  UserKnownHostsFile $dir/etc/known_hosts
  StrictHostKeyChecking accept-new
  HostKeyAlias muxflow-bench-loopback
  ServerAliveInterval 15

Host muxflow-bench
  ProxyCommand perl $here/workloads/delay-proxy.pl 127.0.0.1 $port $dir/delay_ms
EOF

if [[ -s "$dir/sshd.pid" ]]; then
  pid="$(cat "$dir/sshd.pid")"
  if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -qF -- "-f $dir/etc/sshd_config"; then
    kill "$pid"
    sleep 0.5
  fi
fi
env -u TMUX -u TMUX_PANE /usr/bin/sshd -f "$dir/etc/sshd_config" -E "$dir/sshd.log"
for _ in $(seq 50); do
  ssh -F "$dir/ssh_config" -o BatchMode=yes muxflow-bench-direct true 2>/dev/null && break
  sleep 0.1
done
ssh -F "$dir/ssh_config" -o BatchMode=yes muxflow-bench-direct 'echo "loopback sshd up: HOME=$HOME TMUX_TMPDIR=$TMUX_TMPDIR"'
