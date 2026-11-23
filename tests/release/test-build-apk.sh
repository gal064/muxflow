#!/usr/bin/env bash
# Exercise the APK build entrypoint without an Android SDK: prebuild/Gradle
# stand-ins expose its arguments, failure propagation and release checks.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/mobile/scripts" "$work/mobile/android" "$work/bin" "$work/sdk/build-tools/test"
cp "$repo_root/apps/mobile/scripts/"{build-apk,apk-variant,env,verify-apk-crypto}.sh "$work/mobile/scripts/"
cp "$repo_root/apps/mobile/release-cert.sha256" "$work/mobile/"

cat > "$work/bin/pnpm" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'exec expo prebuild --platform android' ]]
STUB
cat > "$work/mobile/android/gradlew" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$QA_GRADLE_ARGS"
if [[ "${QA_GRADLE_FAIL:-}" == true ]]; then
  echo 'Caused by: fixture packaging exception' >&2
  exit 42
fi
case "$1" in
  assembleDebug) apk=app/build/outputs/apk/debug/app-debug.apk ;;
  assembleRelease) apk=app/build/outputs/apk/release/app-release.apk ;;
  *) exit 2 ;;
esac
mkdir -p "$(dirname "$apk")"
printf 'fixture APK\n' > "$apk"
STUB
cat > "$work/sdk/build-tools/test/apksigner" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == verify && "$2" == --print-certs && -f "$3" ]]
touch "$QA_SIGNER_CALLED"
printf 'Signer #1 certificate SHA-256 digest: %s\n' "$QA_CERT"
STUB
cat > "$work/sdk/build-tools/test/dexdump" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ -f "$1" ]]
touch "$QA_DEXDUMP_CALLED"
if [[ "${QA_MISSING_CRYPTO_CLASS:-}" != true ]]; then
  cat <<'DEX'
  Class descriptor  : 'Lorg/bouncycastle/jce/provider/BouncyCastleProvider;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/keystore/BC$Mappings;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/keystore/bc/BcKeyStoreSpi$Std;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/asymmetric/EdEC$Mappings;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/KeyFactorySpi$Ed25519;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/SignatureSpi$Ed25519;'
  Class descriptor  : 'Lorg/bouncycastle/jcajce/provider/asymmetric/edec/KeyAgreementSpi$X25519;'
DEX
fi
STUB
chmod +x "$work/bin/pnpm" "$work/mobile/android/gradlew" "$work/sdk/build-tools/test/"{apksigner,dexdump}
export PATH="$work/bin:$PATH" MUXFLOW_ANDROID_HOME="$work/sdk" MUXFLOW_JAVA_HOME="$work/jdk"
export QA_GRADLE_ARGS="$work/gradle-args" QA_SIGNER_CALLED="$work/signer-called" QA_DEXDUMP_CALLED="$work/dexdump-called"
export QA_CERT
QA_CERT=$(tr -d '[:space:]' < "$work/mobile/release-cert.sha256")
# Test the pinned project certificate rather than inheriting a local override.
unset MUXFLOW_ANDROID_CERT_SHA256

CI=true bash "$work/mobile/scripts/build-apk.sh" release > "$work/release.out"
grep -Fxq assembleRelease "$QA_GRADLE_ARGS"
grep -Fxq -- --stacktrace "$QA_GRADLE_ARGS"
grep -Fxq -- --console=plain "$QA_GRADLE_ARGS"
grep -Fxq -- --no-daemon "$QA_GRADLE_ARGS"
grep -Fxq -- --max-workers=2 "$QA_GRADLE_ARGS"
grep -Fxq -- '-Dorg.gradle.jvmargs=-Xmx4096m -XX:MaxMetaspaceSize=1024m -XX:+ExitOnOutOfMemoryError' "$QA_GRADLE_ARGS"
[[ -f "$QA_SIGNER_CALLED" ]]
grep -q 'Signed by the pinned release certificate' "$work/release.out"
[[ -f "$QA_DEXDUMP_CALLED" ]]
grep -q 'Release APK contains the required HTTPS and SSH crypto classes' "$work/release.out"
echo 'CI release: bounded Gradle settings, pinned certificate and crypto classes verified'

rm "$QA_SIGNER_CALLED" "$QA_DEXDUMP_CALLED"
CI=false bash "$work/mobile/scripts/build-apk.sh" debug > "$work/debug.out"
grep -Fxq assembleDebug "$QA_GRADLE_ARGS"
grep -Fxq -- --stacktrace "$QA_GRADLE_ARGS"
! grep -q -e max-workers -e org.gradle.jvmargs -e no-daemon "$QA_GRADLE_ARGS"
[[ ! -e "$QA_SIGNER_CALLED" ]]
[[ ! -e "$QA_DEXDUMP_CALLED" ]]
echo 'local debug: diagnostics enabled, local resource settings preserved'

status=0
CI=true QA_GRADLE_FAIL=true bash "$work/mobile/scripts/build-apk.sh" release > "$work/failure.out" 2>&1 || status=$?
[[ "$status" == 42 ]]
grep -q 'Caused by: fixture packaging exception' "$work/failure.out"
! grep -q '^APK:' "$work/failure.out"
[[ ! -e "$QA_SIGNER_CALLED" ]]
[[ ! -e "$QA_DEXDUMP_CALLED" ]]
echo 'Gradle failure: original exit code and exception preserved; verification not attempted'

status=0
CI=true QA_CERT=wrong bash "$work/mobile/scripts/build-apk.sh" release > "$work/cert-failure.out" 2>&1 || status=$?
[[ "$status" == 1 ]]
grep -q 'release APK is signed by certificate wrong' "$work/cert-failure.out"
! grep -q '^APK:' "$work/cert-failure.out"
[[ ! -e "$QA_DEXDUMP_CALLED" ]]
echo 'wrong certificate: release rejected'

status=0
CI=true QA_MISSING_CRYPTO_CLASS=true bash "$work/mobile/scripts/build-apk.sh" release > "$work/crypto-failure.out" 2>&1 || status=$?
[[ "$status" == 1 ]]
grep -Fq 'Release APK is missing crypto class' "$work/crypto-failure.out"
! grep -q '^APK:' "$work/crypto-failure.out"
echo 'missing crypto classes: release rejected'
