#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$repo_root"
mkdir -p tmp/ios-evidence
device_id=$(cat tmp/ios-evidence/device-id.txt)
xcrun simctl bootstatus "$device_id" -b
xcrun simctl install "$device_id" tmp/work/ios/Build/Products/Release-iphonesimulator/Muxflow.app
xcrun simctl launch "$device_id" dev.muxflow.mobile | tee tmp/ios-evidence/launch.txt
# A launch screenshot is evidence of startup, not completion of the SSH/UI QA gate.
sleep 5
xcrun simctl io "$device_id" screenshot tmp/ios-evidence/launch.png
xcrun simctl spawn "$device_id" log show --last 2m --style compact --info --debug \
  --predicate 'process == "Muxflow"' > tmp/ios-evidence/app.log
