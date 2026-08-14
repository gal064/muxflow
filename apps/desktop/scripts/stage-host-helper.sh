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
triple="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$triple" ]]
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
staged="$repo/apps/desktop/src-tauri/binaries/tmux-ide-host-$triple"

cargo build --release --manifest-path "$repo/Cargo.toml" --bin tmux-ide-host
mkdir -p "$(dirname "$staged")"
install -m 0755 "$target_dir/release/tmux-ide-host" "$staged"
printf 'staged %s\n' "$staged"
