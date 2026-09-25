#!/usr/bin/env bash
set -euo pipefail

app=${1:-}
[[ -n "$app" && "$app" == /* && -d "$app" && ! -L "$app" ]] || {
  echo "usage: verify-package.sh /absolute/path/to/Muxflow.app" >&2
  exit 64
}

contents="$app/Contents"
plist="$contents/Info.plist"
desktop="$contents/MacOS/muxflow"
host="$contents/MacOS/muxflow-host"
resources="$contents/Resources"
owner='dev.muxflow.desktop:1'

[[ -f "$plist" && -x "$desktop" && -x "$host" ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist") == dev.muxflow.desktop ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist") == 14.0 ]]
if /usr/libexec/PlistBuddy -c 'Print :LSRequiresCarbon' "$plist" >/dev/null 2>&1; then
  echo 'obsolete LSRequiresCarbon metadata must not be present' >&2
  exit 65
fi
grep -Fxq "$owner" "$resources/package-owner"

desktop_file=$(file "$desktop")
host_file=$(file "$host")
[[ "$desktop_file" == *Mach-O*arm64* ]]
[[ "$host_file" == *Mach-O*arm64* ]]

for architecture in aarch64 x86_64; do
  helper="$resources/muxflow-host-linux-$architecture"
  if [[ ${ADE_MACOS_PACKAGE_SMOKE:-0} == 1 && ! -f "$helper" ]]; then
    continue
  fi
  [[ -x "$helper" ]]
  description=$(file "$helper")
  [[ "$description" == *ELF* ]]
  case "$architecture:$description" in
    aarch64:*ARM\ aarch64*|x86_64:*x86-64*) ;;
    *) echo "wrong Linux helper format: $description" >&2; exit 65 ;;
  esac
done

codesign --verify --deep --strict "$app" >/dev/null 2>&1
# MUXFLOW_MACOS_EXPECT_SIGNATURE is set by build-package.sh: `adhoc` for local
# builds, `developer-id` for distributable ones, which must also carry the
# pinned team, the hardened runtime and a secure timestamp on both Mach-O
# binaries, pass Gatekeeper, and hold a stapled notarization ticket.
expect=${MUXFLOW_MACOS_EXPECT_SIGNATURE:-adhoc}
signature_details=$(codesign -dv --verbose=4 "$app" 2>&1)
case "$expect" in
  adhoc)
    [[ "$signature_details" == *'Signature=adhoc'* ]]
    [[ "$signature_details" == *'TeamIdentifier=not set'* ]]
    signature=ad-hoc
    notarization=none
    ;;
  developer-id)
    team=${MUXFLOW_APPLE_TEAM_ID:?developer-id verification requires MUXFLOW_APPLE_TEAM_ID}
    for binary in "$app" "$host"; do
      details=$(codesign -dv --verbose=4 "$binary" 2>&1)
      [[ "$details" == *"Authority=Developer ID Application:"* ]] || { echo "not Developer ID signed: $binary" >&2; exit 65; }
      [[ "$details" == *"TeamIdentifier=$team"* ]] || { echo "wrong team: $binary" >&2; exit 65; }
      [[ "$details" == *'(runtime)'* ]] || { echo "no hardened runtime: $binary" >&2; exit 65; }
      [[ "$details" == *'Timestamp='* ]] || { echo "no secure timestamp: $binary" >&2; exit 65; }
    done
    spctl --assess --type execute "$app" >/dev/null 2>&1 || { echo 'Gatekeeper rejects the app' >&2; exit 65; }
    xcrun stapler validate "$app" >/dev/null || { echo 'no stapled notarization ticket' >&2; exit 65; }
    signature=developer-id
    notarization=stapled
    ;;
  *) echo "unknown MUXFLOW_MACOS_EXPECT_SIGNATURE: $expect" >&2; exit 64 ;;
esac
if spctl --assess --type execute "$app" >/dev/null 2>&1; then
  gatekeeper=accepted
else
  gatekeeper=not-accepted
fi
quarantine=$(xattr -p com.apple.quarantine "$app" 2>/dev/null || true)
[[ -n "$quarantine" ]] || quarantine=absent

printf 'VERIFY_PACKAGE_OK architecture=arm64 signature=%s gatekeeper=%s quarantine=%s notarization=%s\n' \
  "$signature" "$gatekeeper" "$quarantine" "$notarization"
