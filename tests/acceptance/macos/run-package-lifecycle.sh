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
ADE_MACOS_APPLICATIONS_DIR="$applications" release/macos/install.sh >/dev/null
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

# Everything above pins ADE_MACOS_APPLICATIONS_DIR. The unpinned default is
# /Applications, which a hermetic test must never write to, so the default is
# pinned here textually and the per-user location for the cases below is set
# up explicitly. /Applications is only ever read, never written.
grep -Fq 'ADE_MACOS_APPLICATIONS_DIR:-/Applications}' release/macos/install.sh
home="$work/home"
home_applications="$home/Applications"
home_installed="$home_applications/Muxflow.app"
system_installed=/Applications/Muxflow.app
mkdir -p "$home"
HOME="$home" ADE_MACOS_APPLICATIONS_DIR="$home_applications" \
  release/macos/install.sh >/dev/null
[[ -d "$home_installed" ]]
[[ ! -e "$installed" ]]

# A copy left behind in the other well-known location is named, not migrated.
warning=$(HOME="$home" ADE_MACOS_APPLICATIONS_DIR="$applications" \
  release/macos/install.sh 2>&1 >/dev/null)
grep -Fq "$home_installed" <<<"$warning"
[[ -d "$installed" ]]

# Failing between "move the old app aside" and "move the new one in" must leave
# the previous install in place; the backup is the only copy of it at that
# instant, so a cleanup path that skipped the restore would destroy it.
before=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$installed/Contents/Info.plist")
if HOME="$home" ADE_MACOS_APPLICATIONS_DIR="$applications" ADE_PHASE10_TEST_FAIL_BEFORE_PUBLICATION=1 \
  release/macos/install.sh "$candidate" >/dev/null 2>&1; then
  echo "injected pre-publication failure unexpectedly succeeded" >&2
  exit 1
fi
[[ -d "$installed" ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$installed/Contents/Info.plist") == "$before" ]]
[[ -z $(find "$applications" -maxdepth 1 -name '.muxflow.install.*' -print -quit) ]]

HOME="$home" release/macos/uninstall.sh "$applications" >/dev/null
[[ ! -e "$installed" ]]

if grep -Fxq 'dev.muxflow.desktop:1' "$system_installed/Contents/Resources/package-owner" 2>/dev/null; then
  # This machine also carries a machine-wide install, so the no-argument form is
  # genuinely ambiguous: it has to say so rather than pick one or exit silently.
  message=$(HOME="$home" release/macos/uninstall.sh 2>&1 >/dev/null) && status=0 || status=$?
  [[ $status == 64 ]]
  grep -Fq "$home_installed" <<<"$message"
  grep -Fq "$system_installed" <<<"$message"
  [[ -d "$system_installed" ]]
  HOME="$home" release/macos/uninstall.sh "$home_applications" >/dev/null
  [[ -d "$system_installed" ]]
else
  HOME="$home" release/macos/uninstall.sh >/dev/null
  # With nothing left anywhere, the no-argument form names both search paths.
  message=$(HOME="$home" release/macos/uninstall.sh 2>&1 >/dev/null) && status=0 || status=$?
  [[ $status != 0 ]]
  grep -Fq "no Muxflow install found at $home_installed" <<<"$message"
  grep -Fq "no Muxflow install found at $system_installed" <<<"$message"
fi
[[ ! -e "$home_installed" ]]

# Nothing installed at the named location: an explicit message, not a silent
# exit. This form always names a path, so it can never reach a real install.
message=$(HOME="$home" release/macos/uninstall.sh "$home_applications" 2>&1 >/dev/null) && status=0 || status=$?
[[ $status != 0 ]]
grep -Fq "no Muxflow install found at $home_installed" <<<"$message"
grep -Fxq preserve-me "$config_fixture"

echo "PHASE10_PACKAGE_LIFECYCLE_PASS install=clean upgrade=pass rollback=restored uninstall=confined quarantine=preserved default=machine-wide"
