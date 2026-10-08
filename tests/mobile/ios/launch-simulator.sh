#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$repo_root"
mkdir -p tmp/ios-evidence
device_id=$(xcrun simctl list devices available -j | node -e '
  let input = ""; process.stdin.on("data", chunk => input += chunk);
  process.stdin.on("end", () => {
    const devices = Object.values(JSON.parse(input).devices).flat();
    const phone = devices.find(device => device.isAvailable && device.name.startsWith("iPhone"));
    if (!phone) throw new Error("No available iPhone simulator");
    process.stdout.write(phone.udid);
  });')
printf '%s\n' "$device_id" > tmp/ios-evidence/device-id.txt
xcrun simctl boot "$device_id"
xcrun simctl bootstatus "$device_id" -b
xcrun simctl install "$device_id" tmp/work/ios/Build/Products/Release-iphonesimulator/Muxflow.app
xcrun simctl launch "$device_id" dev.muxflow.mobile | tee tmp/ios-evidence/launch.txt
# A launch screenshot is evidence of startup, not completion of the SSH/UI QA gate.
sleep 5
xcrun simctl io "$device_id" screenshot tmp/ios-evidence/launch.png
xcrun simctl spawn "$device_id" log show --last 2m --style compact --info --debug \
  --predicate 'process == "Muxflow"' > tmp/ios-evidence/app.log
