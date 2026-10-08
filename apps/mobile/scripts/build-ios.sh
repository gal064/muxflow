#!/usr/bin/env bash
# Standalone simulator app with local ad hoc signing for Keychain access.
# Distribution certificates and provisioning remain a separate gate.
set -euo pipefail
mobile_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=$(cd "$mobile_root/../.." && pwd)
command -v xcodebuild >/dev/null || { echo 'iOS builds require macOS and Xcode.' >&2; exit 1; }
"$repo_root/release/check-version.sh"
cd "$mobile_root"
pnpm exec expo prebuild --platform ios --no-install
(cd ios && pod install)
export NODE_BINARY
NODE_BINARY=$(command -v node)
# This QA artifact runs on this machine's simulator, so build only its arch.
xcodebuild -workspace ios/Muxflow.xcworkspace -scheme Muxflow \
  -configuration Release -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath "$repo_root/tmp/work/ios" \
  ARCHS="$(uname -m)" ONLY_ACTIVE_ARCH=YES \
  CODE_SIGNING_ALLOWED=YES CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=- build
simulator_app="$repo_root/tmp/work/ios/Build/Products/Release-iphonesimulator/Muxflow.app"
test -f "$simulator_app/Muxflow"
codesign --verify --strict "$simulator_app"
codesign --display --verbose=2 "$simulator_app"
codesign --display --entitlements - --xml "$simulator_app"
