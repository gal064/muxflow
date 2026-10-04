#!/usr/bin/env bash
# Unsigned, standalone simulator app. Distribution signing is a separate gate.
set -euo pipefail
mobile_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=$(cd "$mobile_root/../.." && pwd)
command -v xcodebuild >/dev/null || { echo 'iOS builds require macOS and Xcode.' >&2; exit 1; }
cd "$mobile_root"
pnpm exec expo prebuild --platform ios --no-install
(cd ios && pod install)
export NODE_BINARY
NODE_BINARY=$(command -v node)
xcodebuild -workspace ios/Muxflow.xcworkspace -scheme Muxflow \
  -configuration Release -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath "$repo_root/tmp/work/ios" \
  CODE_SIGNING_ALLOWED=NO build
test -d "$repo_root/tmp/work/ios/Build/Products/Release-iphonesimulator/Muxflow.app"
