#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo"

if [[ -f "$repo/.env" ]]; then
  set -a
  source "$repo/.env"
  set +a
fi
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

work_root=${ADE_WORK_ROOT:-"$repo/tmp/work"}
work_dir="$work_root/macos-package"
mkdir -p "$work_dir"
release/check-disk-space.sh "$repo" "$work_root"
release_target=${CARGO_TARGET_DIR:-"$work_root/cache/release-target/macos"}
[[ "$release_target" == /* ]] || release_target="$repo/$release_target"
export CARGO_TARGET_DIR="$release_target"
mkdir -p "$CARGO_TARGET_DIR"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"

# MUXFLOW_PERF_BUILD=1 compiles the switch-timing instrumentation into this
# package: the host writes `timing.log` and the app records its probes when
# launched with `ADE_PERF_LOG` set. Both are compiled out otherwise, so an
# ordinary package is unchanged. One knob covers the helper, both Linux
# helpers, and the app.
perf_features=()
if [[ ${MUXFLOW_PERF_BUILD:-0} == 1 ]]; then
  perf_features=(--features perf-log)
  echo "perf-log: enabled"
fi
export MUXFLOW_PERF_BUILD

# Two builds, not one: built together, the app's dependencies would unify
# features into the helper's (gpui turns on serde_json's `preserve_order`), and
# the shipped helper must stay the one `-p muxflow-host` builds everywhere else.
# The app is built for the bundle's LSMinimumSystemVersion, as `tauri build`
# did; the helper keeps the toolchain default it always had.
CARGO_INCREMENTAL=0 cargo build --locked --release -p muxflow-host \
  ${perf_features[@]+"${perf_features[@]}"}
CARGO_INCREMENTAL=0 MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release -p muxflow-gpui \
  ${perf_features[@]+"${perf_features[@]}"}

# The bundle is laid out by hand with the names the Tauri bundle used, so an
# install over it keeps its identity: the GPUI binary ships as `muxflow`, the
# helper beside it, and the Linux helpers, icon and owner marker in Resources.
app="$CARGO_TARGET_DIR/release/bundle/macos/Muxflow.app"
version=$(node -e 'process.stdout.write(require("./apps/desktop/src-tauri/tauri.conf.json").version)')
dmg="$CARGO_TARGET_DIR/release/bundle/dmg/Muxflow_${version}_aarch64.dmg"
resources="$app/Contents/Resources"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$resources"
install -m 0755 "$CARGO_TARGET_DIR/release/muxflow-gpui" "$app/Contents/MacOS/muxflow"
install -m 0755 "$CARGO_TARGET_DIR/release/muxflow-host" "$app/Contents/MacOS/muxflow-host"
install -m 0644 apps/desktop/src-tauri/icons/icon.icns release/macos/package-owner "$resources/"
sed "s/@VERSION@/$version/g" release/macos/Info.plist > "$app/Contents/Info.plist"

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
    "$resources/muxflow-host-linux-$helper_arch" >/dev/null
  chmod 0755 "$resources/muxflow-host-linux-$helper_arch"
}

# MUXFLOW_LINUX_HELPERS_DIR supplies helpers that build-compatible-host.sh
# already produced elsewhere (the release workflow builds them on Linux
# runners, since hosted macOS runners have no Docker).
if [[ -n ${MUXFLOW_LINUX_HELPERS_DIR:-} ]]; then
  for helper_arch in aarch64 x86_64; do
    install -m 0755 "$MUXFLOW_LINUX_HELPERS_DIR/muxflow-host-linux-$helper_arch" \
      "$resources/muxflow-host-linux-$helper_arch"
  done
elif [[ ${ADE_MACOS_PACKAGE_SMOKE:-0} != 1 ]]; then
  build_linux_helper aarch64
  build_linux_helper x86_64
fi

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

# Inside out: the nested helper first, then the bundle that seals it.
# macOS UserNotifications requires a stable application identity, so even a
# local build is sealed as a bundle (ad hoc) rather than left linker-signed.
# The hardened runtime needs no entitlements: the app neither JITs nor loads
# unsigned code.
sign=(codesign --force --sign "$identity")
[[ "$identity" == - ]] || sign+=(--options runtime --timestamp)
"${sign[@]}" "$app/Contents/MacOS/muxflow-host"
"${sign[@]}" "$app"
if [[ "$identity" != - ]]; then
  app_zip="$work_dir/Muxflow-$run_id.zip"
  ditto -c -k --keepParent "$app" "$app_zip"
  notarize "$app_zip"
  rm -f "$app_zip"
  "$repo/release/macos/stapler.sh" staple "$app"
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
  "$repo/release/macos/stapler.sh" staple "$dmg"
  "$repo/release/macos/stapler.sh" validate "$dmg"
fi

if [[ "$identity" == - ]]; then
  echo "MACOS_PACKAGE_READY ad-hoc apple-silicon-only app=$app dmg=$dmg"
else
  echo "MACOS_PACKAGE_READY developer-id notarized apple-silicon-only app=$app dmg=$dmg"
fi
