#!/usr/bin/env bash
# Starts the boot and returns; launch-simulator.sh waits for it, so the boot
# overlaps the app build.
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
