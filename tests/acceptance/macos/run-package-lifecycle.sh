#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../../.." && pwd -P)
release_target=${CARGO_TARGET_DIR:-"$repo/tmp/work/cache/release-target/macos"}
source_app="$release_target/release/bundle/macos/Muxflow.app"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
work="$repo/tmp/work/phase10/package-$run_id"
applications="$work/Applications"
candidate="$work/candidate/Muxflow.app"
installed="$applications/Muxflow.app"
config_fixture="$work/config-preserved"

cleanup() {
  [[ "$work" == "$repo/tmp/work/phase10/package-"* ]] && rm -rf "$work"
}
trap cleanup EXIT

[[ $(uname -s) == Darwin && -d "$source_app" ]]
mkdir -p "$applications" "$(dirname "$candidate")"
printf 'preserve-me\n' >"$config_fixture"
ADE_MACOS_APPLICATIONS_DIR="$applications" release/macos/install.sh "$source_app" >/dev/null
ditto --noqtn "$source_app" "$candidate"
/usr/libexec/PlistBuddy -c 'Set :CFBundleVersion 2' "$candidate/Contents/Info.plist"
codesign --force --deep --sign - "$candidate" >/dev/null
xattr -w com.apple.quarantine '0081;00000000;Phase10;' "$candidate"
ADE_MACOS_APPLICATIONS_DIR="$applications" release/macos/install.sh "$candidate" >/dev/null
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$installed/Contents/Info.plist") == 2 ]]
[[ $(xattr -p com.apple.quarantine "$installed") == '0081;00000000;Phase10;' ]]

if ADE_MACOS_APPLICATIONS_DIR="$applications" ADE_PHASE10_TEST_FAIL_AFTER_PUBLICATION=1 \
  release/macos/install.sh "$source_app" >/dev/null 2>&1; then
  echo "injected package upgrade unexpectedly succeeded" >&2
  exit 1
fi
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$installed/Contents/Info.plist") == 2 ]]
grep -Fxq preserve-me "$config_fixture"
ADE_MACOS_APPLICATIONS_DIR="$applications" release/macos/uninstall.sh >/dev/null
[[ ! -e "$installed" ]]
grep -Fxq preserve-me "$config_fixture"

echo "PHASE10_PACKAGE_LIFECYCLE_PASS install=clean upgrade=pass rollback=restored uninstall=confined quarantine=preserved"
