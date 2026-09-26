#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo"

if [[ -f "$repo/.env" ]]; then
  set -a
  source "$repo/.env"
  set +a
fi
# This script owns signing and notarization: tauri's own would sign before the
# Info.plist edit below and so be invalidated by it. Keep tauri's credential
# variables away from `tauri build` whatever the environment or .env holds.
unset APPLE_SIGNING_IDENTITY APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD \
  APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH

# MUXFLOW_MACOS_SIGNING_IDENTITY selects the code identity. Unset or `-` is an
# ad-hoc seal for local builds; a "Developer ID Application: …" identity in the
# keychain produces a distributable build, which also requires the notary API
# key (MUXFLOW_NOTARY_KEY_PATH, MUXFLOW_NOTARY_KEY_ID, MUXFLOW_NOTARY_ISSUER)
# and MUXFLOW_APPLE_TEAM_ID so verification can pin the team.
identity=${MUXFLOW_MACOS_SIGNING_IDENTITY:--}
if [[ "$identity" != - ]]; then
  for required in MUXFLOW_NOTARY_KEY_PATH MUXFLOW_NOTARY_KEY_ID MUXFLOW_NOTARY_ISSUER MUXFLOW_APPLE_TEAM_ID; do
    [[ -n "${!required:-}" ]] || { echo "Developer ID builds require $required" >&2; exit 64; }
  done
  [[ -f "$MUXFLOW_NOTARY_KEY_PATH" ]] || { echo "no notary key at $MUXFLOW_NOTARY_KEY_PATH" >&2; exit 64; }
  export MUXFLOW_MACOS_EXPECT_SIGNATURE=developer-id
else
  export MUXFLOW_MACOS_EXPECT_SIGNATURE=adhoc
fi

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

# MUXFLOW_LINUX_HELPERS_DIR supplies helpers that build-compatible-host.sh
# already produced elsewhere (the release workflow builds them on Linux
# runners, since hosted macOS runners have no Docker).
if [[ -n ${MUXFLOW_LINUX_HELPERS_DIR:-} ]]; then
  for helper_arch in aarch64 x86_64; do
    install -m 0755 "$MUXFLOW_LINUX_HELPERS_DIR/muxflow-host-linux-$helper_arch" \
      "$helpers/muxflow-host-linux-$helper_arch"
  done
elif [[ ${ADE_MACOS_PACKAGE_SMOKE:-0} != 1 ]]; then
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
version=$(node -e 'process.stdout.write(require("./apps/desktop/src-tauri/tauri.conf.json").version)')
dmg="$CARGO_TARGET_DIR/release/bundle/dmg/Muxflow_${version}_aarch64.dmg"

# Submit one file to Apple's notary service and wait. notarytool exits 0 for a
# finished submission whatever its verdict, so the verdict is read explicitly
# and the service's log is printed when it is anything but Accepted.
notarize() {
  local file=$1 result status id
  result=$(xcrun notarytool submit "$file" --wait --output-format json \
    --key "$MUXFLOW_NOTARY_KEY_PATH" --key-id "$MUXFLOW_NOTARY_KEY_ID" --issuer "$MUXFLOW_NOTARY_ISSUER")
  status=$(node -e 'process.stdout.write(JSON.parse(process.argv[1]).status ?? "")' "$result")
  if [[ "$status" != Accepted ]]; then
    id=$(node -e 'process.stdout.write(JSON.parse(process.argv[1]).id ?? "")' "$result")
    echo "notarization of $(basename "$file") returned ${status:-no status}: $result" >&2
    [[ -z "$id" ]] || xcrun notarytool log "$id" \
      --key "$MUXFLOW_NOTARY_KEY_PATH" --key-id "$MUXFLOW_NOTARY_KEY_ID" --issuer "$MUXFLOW_NOTARY_ISSUER" >&2 || true
    exit 1
  fi
}

# macOS UserNotifications requires a stable application identity, and the
# plist edit breaks the bundler's seal, so the bundle is always re-signed here.
/usr/libexec/PlistBuddy -c 'Delete :LSRequiresCarbon' "$app/Contents/Info.plist" 2>/dev/null || true
if [[ "$identity" == - ]]; then
  codesign --force --sign - "$app"
else
  # Inside out: the nested helper first, then the bundle that seals it. The
  # hardened runtime needs no entitlements; the app loads no unsigned code and
  # JIT runs in WebKit's own processes.
  codesign --force --options runtime --timestamp --sign "$identity" "$app/Contents/MacOS/muxflow-host"
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
  app_zip="$work_dir/Muxflow-$run_id.zip"
  ditto -c -k --keepParent "$app" "$app_zip"
  notarize "$app_zip"
  rm -f "$app_zip"
  xcrun stapler staple "$app"
fi
"$repo/release/macos/verify-package.sh" "$app"

mkdir -p "$(dirname "$dmg")"
# dmgbuild writes the Finder layout directly instead of scripting Finder, so the
# drag-to-install window comes out the same on a headless runner. Pinned so the
# layout cannot drift between releases.
command -v uvx >/dev/null || { echo "building the DMG requires uv (https://docs.astral.sh/uv/)" >&2; exit 69; }
uvx --from dmgbuild==1.6.7 dmgbuild -s "$repo/release/macos/dmg-settings.py" -D app="$app" Muxflow "$dmg"
if [[ "$identity" != - ]]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  notarize "$dmg"
  xcrun stapler staple "$dmg"
  xcrun stapler validate "$dmg"
fi

cleanup
trap - EXIT
if [[ "$identity" == - ]]; then
  echo "MACOS_PACKAGE_READY ad-hoc apple-silicon-only app=$app dmg=$dmg"
else
  echo "MACOS_PACKAGE_READY developer-id notarized apple-silicon-only app=$app dmg=$dmg"
fi
