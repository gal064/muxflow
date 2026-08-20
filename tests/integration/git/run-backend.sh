#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase5 backend gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
runtime="$repo_root/tmp/phase5-backend-$run_id"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/integration/git/protocol-driver/target}"
host_binary="$main_target/debug/muxflow-host"
driver_binary="$driver_target/debug/git-test-driver"
socket_name="ade-phase5-local-$$"
host_runtime="$runtime/host-runtime"
fixture="$runtime/repo"
cleanup() {
  ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  tmux -L "$socket_name" kill-server >/dev/null 2>&1 || true
}
trap cleanup EXIT
mkdir -p "$runtime"
printf '%s\n' "$runtime" >"$repo_root/tmp/phase5-backend-latest"

cd "$repo_root"
cargo fmt --all -- --check >"$runtime/fmt.log" 2>&1 &
fmt_pid=$!
cargo test -p tmux-agent-protocol --test roundtrip >"$runtime/protocol.log" 2>&1 &
protocol_pid=$!
cargo test -p muxflow-host service::git::tests -- --test-threads=1 >"$runtime/host-git.log" 2>&1 &
host_pid=$!
cargo test -p muxflow connection::git::tests >"$runtime/desktop-bridge.log" 2>&1 &
bridge_pid=$!
cargo build --bin muxflow-host >"$runtime/host-build.log" 2>&1 &
host_build_pid=$!
cargo build --manifest-path tests/integration/git/protocol-driver/Cargo.toml >"$runtime/driver-build.log" 2>&1 &
driver_build_pid=$!

wait "$fmt_pid"
wait "$protocol_pid"
wait "$host_pid"
wait "$bridge_pid"
wait "$host_build_pid"
wait "$driver_build_pid"

mkdir -p "$host_runtime" "$fixture"
chmod 0700 "$host_runtime"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Phase Five'
git -C "$fixture" config user.email phase5@example.test
printf '%s\n' 'ignored*' >"$fixture/.gitignore"
printf '%s\n' base >"$fixture/tracked"
git -C "$fixture" add .gitignore tracked
git -C "$fixture" commit -qm base
printf '%s\n' changed >"$fixture/tracked"
printf '%s\n' ignored >"$fixture/ignored-one"
printf '%s\n' literal >"$fixture/:(glob)*"
printf '%s\n' ordinary >"$fixture/ordinary"
# `raw-\377` is deliberately not valid UTF-8, which is the point of the fixture.
# APFS enforces valid UTF-8 in filenames and refuses to create it at all
# ("Illegal byte sequence"), so this local route substitutes a non-ASCII UTF-8
# name and tells the driver which bytes to expect. The invalid-UTF-8 case is not
# lost: the Docker/SSH route builds its repository inside the Linux container
# and still uses `raw-\377` unchanged.
if phase8_is_darwin; then
  raw_name="$(printf 'raw-\303\251')"
  raw_name_hex=7261772dc3a9
else
  raw_name="$(printf 'raw-\377')"
  raw_name_hex=7261772dff
fi
export ADE_PHASE5_RAW_NAME_HEX="$raw_name_hex"
printf '%s\n' raw >"$fixture/$raw_name"
printf '%s\n' '#!/bin/sh' 'echo phase5-hook-blocked >&2' 'exit 17' >"$fixture/.git/hooks/pre-commit"
chmod 0700 "$fixture/.git/hooks/pre-commit"
tmux -L "$socket_name" new-session -d -s phase5 -c "$fixture" 'exec bash'
ADE_HOST_RUNTIME_DIR="$host_runtime" ADE_TMUX_SOCKET_NAME="$socket_name" \
  "$driver_binary" local "$host_binary" >"$runtime/local.json"
jq -e '.authoritativeStatus and .repositoryStableAcrossReconnect and .rawPathSafe and .ignored and .stage and .stagedDiff and .hookErrorSurfaced and .discardTokenEnforced and .pathEscapeRejected and .staleConnectionRejected and .literalPathspecSafe and .transportLossCancelledHook' \
  "$runtime/local.json" >/dev/null

cargo clippy -p muxflow-host -p tmux-agent-protocol -p muxflow --all-targets -- -D warnings \
  >"$runtime/clippy.log" 2>&1 &
clippy_pid=$!
wait "$clippy_pid"

git --version >"$runtime/git-version.txt"
tmux -V >"$runtime/tmux-version.txt"
printf '%s\n' "phase5 backend evidence: $runtime"
