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
signature_details=$(codesign -dv --verbose=4 "$app" 2>&1)
[[ "$signature_details" == *'Signature=adhoc'* ]]
[[ "$signature_details" == *'TeamIdentifier=not set'* ]]
signature=ad-hoc-internal
if spctl --assess --type execute "$app" >/dev/null 2>&1; then
  gatekeeper=accepted
else
  gatekeeper=not-accepted
fi
quarantine=$(xattr -p com.apple.quarantine "$app" 2>/dev/null || true)
[[ -n "$quarantine" ]] || quarantine=absent

printf 'VERIFY_PACKAGE_OK architecture=arm64 signature=%s gatekeeper=%s quarantine=%s notarization=not-claimed\n' \
  "$signature" "$gatekeeper" "$quarantine"
