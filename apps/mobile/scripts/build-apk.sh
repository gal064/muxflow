#!/usr/bin/env bash
# Build an APK locally: prebuild the Android project, then assemble it.
# Takes a variant, debug (default) or release. The generated `android/`
# directory is disposable and is not committed.
#
# The release variant is signed with the Muxflow release key (see
# plugins/withReleaseSigning.js for the MUXFLOW_ANDROID_* variables) and then
# checked against the certificate pinned in release-cert.sha256, so a release
# APK signed by anything else never leaves this script.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=./env.sh
source ./scripts/env.sh
# shellcheck source=./apk-variant.sh
source ./scripts/apk-variant.sh "${1:-debug}"

pnpm exec expo prebuild --platform android
(cd android && ./gradlew "$APK_GRADLE_TASK")

if [[ "$APK_VARIANT" == release ]]; then
  expected=${MUXFLOW_ANDROID_CERT_SHA256:-}
  if [[ -z "$expected" ]]; then
    [[ -f release-cert.sha256 ]] || { echo "no pinned release certificate: apps/mobile/release-cert.sha256" >&2; exit 1; }
    expected=$(tr -d '[:space:]' < release-cert.sha256)
  fi
  # keytool prints the digest as colon-separated pairs, apksigner as bare hex.
  expected=${expected//:/}
  apksigner=$(ls -d "$ANDROID_HOME"/build-tools/*/apksigner | sort -V | tail -n 1)
  actual=$("$apksigner" verify --print-certs "$APK_PATH" \
    | sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
  if [[ "${actual,,}" != "${expected,,}" ]]; then
    echo "release APK is signed by certificate ${actual:-<none>}, expected $expected" >&2
    exit 1
  fi
  echo "Signed by the pinned release certificate: $actual"
fi

echo "APK: $APK_PATH"
