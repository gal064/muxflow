#!/usr/bin/env bash
# Build an APK locally: prebuild the Android project, then assemble it.
# Takes a variant, debug (default) or release. The generated `android/`
# directory is disposable and is not committed.
#
# The Expo template signs the release variant with the debug keystore, so a
# release APK sideloads with install-apk.sh but is not a distributable
# signing identity.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
source ./scripts/env.sh
# shellcheck source=./apk-variant.sh
source ./scripts/apk-variant.sh "${1:-debug}"

pnpm exec expo prebuild --platform android
(cd android && ./gradlew "$APK_GRADLE_TASK")

echo "APK: $APK_PATH"
