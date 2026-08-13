#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo"

[[ $(uname -s) == Darwin ]] || { echo "macOS packaging requires Darwin" >&2; exit 69; }
[[ $(uname -m) == arm64 ]] || { echo "this internal package is Apple Silicon only" >&2; exit 69; }

helpers="$repo/apps/desktop/src-tauri/binaries/linux-helpers"
mkdir -p "$helpers" "$repo/tmp/work/phase10"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
package_targets="$repo/tmp/work/phase10/.package-targets-$run_id"
cleanup() {
  [[ "$package_targets" == "$repo/tmp/work/phase10/.package-targets-$run_id" ]] || return
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

CARGO_INCREMENTAL=0 cargo build --locked --release -p tmux-ide-host

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
    "$helpers/tmux-ide-host-linux-$helper_arch" >/dev/null
  chmod 0755 "$helpers/tmux-ide-host-linux-$helper_arch"
}

if [[ ${ADE_MACOS_PACKAGE_SMOKE:-0} != 1 ]]; then
  build_linux_helper aarch64
  build_linux_helper x86_64
fi

pnpm --dir apps/desktop tauri build --bundles app
app="$repo/target/release/bundle/macos/tmux Agent IDE.app"
install -m 0755 "$repo/target/release/tmux-ide-host" "$app/Contents/MacOS/tmux-ide-host"
# Tauri signs before the helper replacement above, so its bundle seal is no
# longer valid here. Re-seal with an ad-hoc identity: macOS UserNotifications
# requires a stable application identity even for an internal build, while
# this still makes no Developer ID, Gatekeeper, or notarization claim.
/usr/libexec/PlistBuddy -c 'Delete :LSRequiresCarbon' "$app/Contents/Info.plist" 2>/dev/null || true
codesign --force --sign - "$app/Contents/MacOS/tmux-ide-host"
codesign --force --sign - "$app"
"$repo/release/macos/verify-package.sh" "$app"
dmg="$repo/target/release/bundle/dmg/tmux Agent IDE_0.1.0_aarch64.dmg"
mkdir -p "$(dirname "$dmg")"
hdiutil create -volname 'tmux Agent IDE' -srcfolder "$app" -ov -format UDZO "$dmg"

cleanup
trap - EXIT
echo "MACOS_PACKAGE_READY unsigned-internal apple-silicon-only"
