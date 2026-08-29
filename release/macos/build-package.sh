#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo"

[[ $(uname -s) == Darwin ]] || { echo "macOS packaging requires Darwin" >&2; exit 69; }
[[ $(uname -m) == arm64 ]] || { echo "this internal package is Apple Silicon only" >&2; exit 69; }

helpers="$repo/apps/desktop/src-tauri/binaries/linux-helpers"
work_root=${ADE_WORK_ROOT:-"$repo/tmp/work"}
work_dir="$work_root/macos-package"
mkdir -p "$helpers" "$work_dir"
release/check-disk-space.sh "$repo" "$work_root"
release_target=${CARGO_TARGET_DIR:-"$work_root/cache/release-target/macos"}
[[ "$release_target" == /* ]] || release_target="$repo/$release_target"
export CARGO_TARGET_DIR="$release_target"
mkdir -p "$CARGO_TARGET_DIR"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
package_targets="$work_dir/.package-targets-$run_id"
cleanup() {
  [[ "$package_targets" == "$work_dir/.package-targets-$run_id" ]] || return
  # Finder can create .DS_Store concurrently in browsed directories. The
  # hidden per-run parent avoids that normal path and bounded retries keep an
  # exact late write from making an otherwise successful package build fail.
  for _ in 1 2 3; do
    rm -rf "$package_targets"
    [[ ! -e "$package_targets" ]] && return
  done
  echo "failed to clean package target $package_targets" >&2
  return 1
}
trap cleanup EXIT

# MUXFLOW_PERF_BUILD=1 compiles the switch-timing instrumentation into this
# package: the host writes `timing.log` and the app answers `perf_log_enabled`
# when launched with `ADE_PERF_LOG` set. Both are compiled out otherwise, so an
# ordinary package is unchanged. One knob covers the helper, both Linux
# helpers, and the app.
perf_features=()
if [[ ${MUXFLOW_PERF_BUILD:-0} == 1 ]]; then
  perf_features=(--features perf-log)
  echo "perf-log: enabled"
fi
export MUXFLOW_PERF_BUILD

CARGO_INCREMENTAL=0 cargo build --locked --release -p muxflow-host \
  ${perf_features[@]+"${perf_features[@]}"}

# The shipped Linux helpers are produced by release/linux/build-compatible-host.sh,
# the same builder every Phase 8/9 gate validates. They previously had their own
# recipe here, which diverged from it in four ways that all mattered: a floating
# `rust:1.97-bookworm` tag instead of the pinned 1.97.1 the toolchain file
# declares, no SOURCE_DATE_EPOCH, no `--remap-path-prefix`, and a read-write
# repository mount. The result was a helper that shipped without being
# reproducible and that was not the artifact any gate had exercised — two clean
# builds of the tested recipe are byte-identical, while this path produced
# different bytes from identical source (M10-E048).
build_linux_helper() {
  local helper_arch=$1
  release/linux/build-compatible-host.sh "$helper_arch" \
    "$helpers/muxflow-host-linux-$helper_arch" >/dev/null
  chmod 0755 "$helpers/muxflow-host-linux-$helper_arch"
}

if [[ ${ADE_MACOS_PACKAGE_SMOKE:-0} != 1 ]]; then
  build_linux_helper aarch64
  build_linux_helper x86_64
fi

# The helper is staged by the build hook and copied into Contents/MacOS by
# tauri's own externalBin handling, so this flow and a plain `tauri build` ship
# the same sidecar by the same mechanism. It used to be installed and re-signed
# by hand here, which left the bundler's path exercised only by the bare flow.
# `--features` is the tauri CLI's own flag for the app crate's cargo features,
# which is why the feature is named here rather than passed through after `--`.
if [[ ${MUXFLOW_PERF_BUILD:-0} == 1 ]]; then
  pnpm --dir apps/desktop tauri build --bundles app --features perf-log
else
  pnpm --dir apps/desktop tauri build --bundles app
fi
app="$CARGO_TARGET_DIR/release/bundle/macos/Muxflow.app"
# Re-seal with an ad-hoc identity after the plist edit: macOS UserNotifications
# requires a stable application identity even for an internal build, while this
# still makes no Developer ID, Gatekeeper, or notarization claim.
/usr/libexec/PlistBuddy -c 'Delete :LSRequiresCarbon' "$app/Contents/Info.plist" 2>/dev/null || true
codesign --force --sign - "$app"
"$repo/release/macos/verify-package.sh" "$app"
version=$(node -e 'process.stdout.write(require("./apps/desktop/src-tauri/tauri.conf.json").version)')
dmg="$CARGO_TARGET_DIR/release/bundle/dmg/Muxflow_${version}_aarch64.dmg"
mkdir -p "$(dirname "$dmg")"
hdiutil create -volname 'Muxflow' -srcfolder "$app" -ov -format UDZO "$dmg"

cleanup
trap - EXIT
echo "MACOS_PACKAGE_READY unsigned-internal apple-silicon-only app=$app dmg=$dmg"
