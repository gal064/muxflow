#!/usr/bin/env bash
set -euo pipefail

applications=${ADE_MACOS_APPLICATIONS_DIR:-"$HOME/Applications"}
name='Muxflow.app'
owner='dev.muxflow.desktop:1'
[[ "$applications" == /* && "$applications" != / ]]
applications=$(cd "$applications" && pwd -P)
target="$applications/$name"
[[ -d "$target" && ! -L "$target" ]]
grep -Fxq "$owner" "$target/Contents/Resources/package-owner"
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$target/Contents/Info.plist") == dev.muxflow.desktop ]]

host="$target/Contents/MacOS/muxflow-host"
if [[ -x "$host" ]]; then
  # Gatekeeper SIGKILLs a helper executed from a still-quarantined ad-hoc
  # bundle, so an unconditional `|| true` here silently orphans a running
  # daemon. The bundle is about to be removed anyway: clear the attribute
  # first, and if the graceful stop still fails, terminate the daemon this
  # exact bundle owns rather than reporting a success that did not happen.
  xattr -dr com.apple.quarantine "$target" >/dev/null 2>&1 || true
  if ! "$host" daemon-stop >/dev/null 2>&1; then
    pkill -f "^$host daemon " >/dev/null 2>&1 || true
  fi
fi

umask 077
quarantine=$(mktemp -d "$applications/.muxflow.uninstall.XXXXXX")
mv "$target" "$quarantine/$name"
rm -rf "$quarantine"
printf 'Removed Muxflow. tmux sessions and user configuration were preserved.\n'
