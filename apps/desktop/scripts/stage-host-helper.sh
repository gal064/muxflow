#!/usr/bin/env bash
# Stages the host helper where Tauri's `externalBin` expects it, so that a plain
# `tauri build` produces an app that can connect locally (P12-U004).
#
# Before this, only release/macos/build-package.sh copied the helper into the
# bundle, so an app built the ordinary way came out without its sidecar and
# refused every local connection. The bundler takes one file per target triple
# and drops the suffix when it copies it into Contents/MacOS.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
# Tauri sets this for its build hooks; it is the target the bundle is being
# built for, which is not the host triple under `--target` or a universal build.
triple="${TAURI_ENV_TARGET_TRIPLE:-$(rustc -vV | sed -n 's/^host: //p')}"
[[ -n "$triple" ]]
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
staged="$repo/apps/desktop/src-tauri/binaries/muxflow-host-$triple"

# MUXFLOW_PERF_BUILD=1 compiles the host's switch-timing instrumentation into
# this otherwise ordinary release helper, so a measurement build of the app
# ships a helper that writes `timing.log`. Off by default; see the compile-out
# note in apps/host/src/diagnostics.rs.
perf_features=()
if [[ ${MUXFLOW_PERF_BUILD:-0} == 1 ]]; then
  perf_features=(--features perf-log)
  printf 'perf-log: enabled (host helper)\n'
fi

# `--locked`, like the release flow: a packaged helper must be built from the
# dependency versions the lockfile pins, not from whatever resolves today.
cargo build --locked --release --manifest-path "$repo/Cargo.toml" --bin muxflow-host \
  ${perf_features[@]+"${perf_features[@]}"}
mkdir -p "$(dirname "$staged")"
install -m 0755 "$target_dir/release/muxflow-host" "$staged"
printf 'staged %s\n' "$staged"
