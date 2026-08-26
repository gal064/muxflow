#!/usr/bin/env bash
# Build a debug APK locally: prebuild the Android project, then assemble it.
# The generated `android/` directory is disposable and is not committed.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
source ./scripts/env.sh

pnpm exec expo prebuild --platform android
(cd android && ./gradlew assembleDebug)

echo "APK: $(pwd)/android/app/build/outputs/apk/debug/app-debug.apk"
