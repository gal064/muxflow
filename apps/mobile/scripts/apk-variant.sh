#!/usr/bin/env bash
# Resolve an APK variant into the Gradle task, output path and build command
# that build-apk.sh and install-apk.sh both need. Source it from the package
# root, after env.sh:
#
#     source ./scripts/apk-variant.sh "${1:-debug}"
#
# Sets APK_VARIANT, APK_GRADLE_TASK, APK_PATH and APK_BUILD_CMD.

case "${1:-debug}" in
  debug)
    APK_VARIANT=debug
    APK_GRADLE_TASK=assembleDebug
    APK_BUILD_CMD='pnpm mobile:apk'
    ;;
  release)
    APK_VARIANT=release
    APK_GRADLE_TASK=assembleRelease
    APK_BUILD_CMD='pnpm mobile:apk:release'
    ;;
  *)
    echo "unknown APK variant: $1 (want debug or release)" >&2
    exit 64
    ;;
esac

APK_PATH="$PWD/android/app/build/outputs/apk/$APK_VARIANT/app-$APK_VARIANT.apk"
