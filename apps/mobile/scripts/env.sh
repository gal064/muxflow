#!/usr/bin/env bash
# Android/Java toolchain for Muxflow Mobile.
#
# Source it, do not execute it:
#
#     source apps/mobile/scripts/env.sh
#
# It only exports variables; it never edits your shell rc files. See
# docs/mobile/toolchain.md for what these paths contain and how they were
# installed.
#
# The exports are unconditional on purpose. The point of the file is to pin the
# toolchain the Gradle build is known to work with, and the common failure it
# exists to prevent is a distro JDK (21/24/25) already on JAVA_HOME making the
# build fail somewhere far from the cause. Override by exporting
# MUXFLOW_JAVA_HOME / MUXFLOW_ANDROID_HOME instead.

export JAVA_HOME="${MUXFLOW_JAVA_HOME:-$HOME/.local/jdk-17}"
export ANDROID_HOME="${MUXFLOW_ANDROID_HOME:-$HOME/Android/Sdk}"
# Some tools still read the deprecated name; keep the two in agreement.
export ANDROID_SDK_ROOT="$ANDROID_HOME"

for _muxflow_dir in \
  "$JAVA_HOME/bin" \
  "$ANDROID_HOME/cmdline-tools/latest/bin" \
  "$ANDROID_HOME/platform-tools" \
  "$ANDROID_HOME/emulator"; do
  case ":$PATH:" in
    *":$_muxflow_dir:"*) ;;
    *) PATH="$_muxflow_dir:$PATH" ;;
  esac
done
unset _muxflow_dir
export PATH
