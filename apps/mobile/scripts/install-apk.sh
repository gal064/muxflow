#!/usr/bin/env bash
# Install an already-built APK on the connected device with adb.
# Takes a variant, debug (default) or release; build it first with build-apk.sh.
#
# Debug builds are signed with the debug keystore and release builds with the
# release key, so -r upgrades a build of the same variant in place, but
# switching between debug and release needs an uninstall first.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
source ./scripts/env.sh
# shellcheck source=./apk-variant.sh
source ./scripts/apk-variant.sh "${1:-debug}"

if [[ ! -f "$APK_PATH" ]]; then
  echo "no $APK_VARIANT APK at $APK_PATH" >&2
  echo "build it first: $APK_BUILD_CMD" >&2
  exit 1
fi

echo "Installing $APK_VARIANT APK: $APK_PATH"
adb install -r "$APK_PATH"
