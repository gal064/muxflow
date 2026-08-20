#!/usr/bin/env bash
# Prepares an isolated, instrumented app run for the Phase 12 GUI pass.
#
# Everything mutable is disposable and uniquely named: an `ade-phase12-*` tmux
# socket under a private TMUX_TMPDIR, private HOME/XDG roots, and a scratch
# repository. The user's own tmux server and profiles are never touched. The
# app is launched with ADE_PERF_LOG set, which is the only way the in-app
# latency probe turns on.
#
# Usage:
#   bash tests/performance/runtime/setup-qa.sh            # local fixture
#   ADE_PHASE12_QA_REMOTE=remote-linux bash tests/performance/runtime/setup-qa.sh
#
# Prints the run directory; `<run>/launch.sh` starts the app.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_root="$repo_root/tmp/phase12-qa-$run_id"
home="$run_root/home"
config="$run_root/config"
data="$run_root/data"
cache="$run_root/cache"
runtime="$run_root/runtime"
tmux_runtime="$run_root/tmux"
repository="$run_root/repository"
tmux_socket="ade-phase12-qa-$$"
# Overridable so a lane can be run against a bundle built somewhere other than
# the default target directory. The user runs this same app against their own
# tmux server while these lanes run, and rebuilding `target/release` replaces
# the bundle out from under their live window; building the QA bundle into its
# own target directory leaves theirs alone.
target_root="${ADE_PHASE12_QA_TARGET:-$repo_root/target}"
desktop="${ADE_PHASE12_QA_DESKTOP:-$target_root/release/bundle/macos/Muxflow.app/Contents/MacOS/muxflow}"
helper="${ADE_PHASE12_QA_HELPER:-$target_root/release/muxflow-host}"

[[ -x "$desktop" ]] || { echo "build the app bundle first: pnpm --filter @muxflow/desktop tauri build --bundles app" >&2; exit 1; }
[[ -x "$helper" ]] || { echo "build the release helper first: cargo build --release --bin muxflow-host" >&2; exit 1; }

mkdir -p "$home" "$config" "$data" "$cache" "$runtime" "$tmux_runtime" "$repository" "$run_root/evidence"
chmod 0700 "$home" "$config" "$data" "$cache" "$runtime" "$tmux_runtime"

# A repository with enough breadth to exercise the Explorer and Git surfaces.
git -C "$repository" init -q
git -C "$repository" config user.name 'Phase Twelve QA'
git -C "$repository" config user.email phase12@example.test
printf '# Phase 12\n\nPerformance QA fixture.\n' >"$repository/README.md"
printf 'baseline\n' >"$repository/tracked.txt"
printf 'ignored.log\n' >"$repository/.gitignore"
mkdir -p "$repository/wide"
for index in $(seq 1 4096); do
  printf 'entry %s\n' "$index" >"$repository/wide/file-$(printf '%04d' "$index").txt"
done
git -C "$repository" add .
git -C "$repository" commit -qm baseline
printf 'working tree change\n' >>"$repository/tracked.txt"
printf 'ignored\n' >"$repository/ignored.log"

remote_profile=''
if [[ -n "${ADE_PHASE12_QA_REMOTE:-}" ]]; then
  # The user's own ssh config, agent and known_hosts are used as-is; nothing
  # about the SSH setup is written or modified by this script.
  remote_profile=$(printf ',\n    {"id":"phase12-remote","label":"Phase 12 Remote","connection":{"mode":"ssh","profileId":"phase12-remote","target":"%s"}}' "$ADE_PHASE12_QA_REMOTE")
fi
# Seeded into *both* config roots, because the app does not use XDG on macOS:
# `app_config_dir()` is `$HOME/Library/Application Support/<identifier>` there
# and `$XDG_CONFIG_HOME/<identifier>` on Linux. Writing only the XDG path meant
# every macOS run of this fixture silently started with the default single
# Local profile — `ADE_PHASE12_QA_REMOTE` did nothing at all, and no lane that
# needs a remote host could ever have used it.
for profile_dir in \
  "$config/dev.muxflow.desktop" \
  "$home/Library/Application Support/dev.muxflow.desktop"
do
  mkdir -p "$profile_dir"
  printf '{\n  "schemaVersion": 1,\n  "profiles": [\n    {"id":"local","label":"Local","connection":{"mode":"local"}}%s\n  ],\n  "lastProfileId":"local"\n}\n' "$remote_profile" >"$profile_dir/profiles.json"
  chmod 0600 "$profile_dir/profiles.json"
done

cat >"$run_root/launch.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export HOME='$home'
export XDG_CONFIG_HOME='$config'
export XDG_DATA_HOME='$data'
export XDG_CACHE_HOME='$cache'
export XDG_RUNTIME_DIR='$runtime'
export TMUX_TMPDIR='$tmux_runtime'
export ADE_TMUX_SOCKET_NAME='$tmux_socket'
export ADE_HOST_HELPER_PATH='$helper'
export ADE_PERF_LOG='$run_root/evidence/perf.jsonl'
unset TMUX
if ! tmux -L '$tmux_socket' list-sessions >/dev/null 2>&1; then
  tmux -L '$tmux_socket' -f /dev/null new-session -d -s phase12 -c '$repository' 'exec bash'
  tmux -L '$tmux_socket' -f /dev/null new-window -d -t phase12 -n second -c '$repository' 'exec bash'
fi
# The pid this fixture is allowed to kill, and the only one. \`exec\` keeps it.
printf '%s\n' "\$\$" >'$run_root/desktop.pid'
exec '$desktop' >>'$run_root/desktop.log' 2>&1
EOF
chmod 0700 "$run_root/launch.sh"

cat >"$run_root/cleanup.sh" <<EOF
#!/usr/bin/env bash
set -uo pipefail
# By pid, never by name. The user runs this same bundle against their own tmux
# server while these lanes run, and \`pkill -f '<bundle path>'\` matches their
# window exactly as well as the fixture's — it has closed the user's app, with
# their real session attached, mid-lane.
#
# The pid is checked against the process actually wearing it, and the file is
# removed, so a re-run of this script — or a stale file from a run whose app
# already exited — cannot signal whatever inherited the number in between.
if [[ -r '$run_root/desktop.pid' ]]; then
  ade_pid="\$(cat '$run_root/desktop.pid')"
  if [[ "\$ade_pid" =~ ^[0-9]+\$ ]] && ps -p "\$ade_pid" -o command= 2>/dev/null | grep -qF 'muxflow'; then
    kill "\$ade_pid" >/dev/null 2>&1 || true
  fi
  rm -f '$run_root/desktop.pid'
fi
# The same resolution the app was launched with. Naming the directory here
# instead resolved $runtime while the app's daemon, which only had
# XDG_RUNTIME_DIR, was in $runtime/muxflow — so cleanup could not
# reach it and every run left a daemon behind.
HOME='$home' XDG_RUNTIME_DIR='$runtime' \\
  ADE_TMUX_SOCKET_NAME='$tmux_socket' '$helper' daemon-stop >/dev/null 2>&1 || true
env -u TMUX HOME='$home' TMUX_TMPDIR='$tmux_runtime' tmux -L '$tmux_socket' kill-server >/dev/null 2>&1 || true
EOF
chmod 0700 "$run_root/cleanup.sh"

printf '%s\n' "$run_root" >"$repo_root/tmp/phase12-qa-latest"
printf '%s\n' "$run_root"
