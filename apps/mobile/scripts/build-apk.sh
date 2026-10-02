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
# Preserve the underlying packaging exception in local and CI logs.
gradle_args=("$APK_GRADLE_TASK" --stacktrace --console=plain)
if [[ "${CI:-}" == true ]]; then
  # Expo's generated 512 MiB metadata limit was exhausted on hosted runners.
  # Keep the heap at 2 GiB and leave room for Kotlin, Metro and native builds.
  gradle_args+=(--no-daemon --max-workers=2 "-Dorg.gradle.jvmargs=-Xmx2048m -XX:MaxMetaspaceSize=1024m")
fi
(cd android && ./gradlew "${gradle_args[@]}")

if [[ "$APK_VARIANT" == release ]]; then
  expected=${MUXFLOW_ANDROID_CERT_SHA256:-}
  if [[ -z "$expected" ]]; then
    [[ -f release-cert.sha256 ]] || { echo "no pinned release certificate: apps/mobile/release-cert.sha256" >&2; exit 1; }
    expected=$(tr -d '[:space:]' < release-cert.sha256)
  fi
  # keytool prints the digest as colon-separated pairs, apksigner as bare hex.
  expected=${expected//:/}
  apksigner=$(ls -d "$ANDROID_HOME"/build-tools/*/apksigner | sort -V | tail -n 1)
  # The signer label varies by build-tools version ("Signer #1", "V2 Signer:"),
  # so collect every signer's digest and require exactly one distinct value.
  actual=$("$apksigner" verify --print-certs "$APK_PATH" \
    | sed -n 's/^.*[Ss]igner.* certificate SHA-256 digest: //p' | sort -u)
  if [[ "${actual,,}" != "${expected,,}" ]]; then
    echo "release APK is signed by certificate ${actual:-<none>}, expected $expected" >&2
    exit 1
  fi
  echo "Signed by the pinned release certificate: $actual"
fi

echo "APK: $APK_PATH"
