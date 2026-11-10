#!/usr/bin/env bash
# Disposable workaround for gpui-pre-macos 0.3.11 on the pinned stable compiler.
# Only Apache-2.0 platform code is copied; no editor/terminal/workspace sources.
set -euo pipefail
cd "$(dirname "$0")"
cargo fetch --target aarch64-apple-darwin
crate=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name gpui-pre-macos-0.3.11 -print -quit)
[[ -n "$crate" ]]
mkdir -p target/patched-macos
cp -R "$crate/." target/patched-macos/
perl -pi -e 's/\.map_or_default\(/.map_or_else(Default::default, /g' target/patched-macos/src/window.rs
printf 'Build with: cargo --config '\''patch.crates-io.gpui-pre-macos.path="target/patched-macos"'\'' build --workspace\n'
