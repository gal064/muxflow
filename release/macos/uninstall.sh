#!/usr/bin/env bash
set -euo pipefail

name='Muxflow.app'
owner='dev.muxflow.desktop:1'

(( $# <= 1 )) || {
  echo "usage: uninstall.sh [/absolute/path/to/Applications]" >&2; exit 64;
}

owned() {
  [[ -d "$1/$name" && ! -L "$1/$name" ]] || return 1
  grep -Fxq "$owner" "$1/$name/Contents/Resources/package-owner" 2>/dev/null
}

applications=${1:-${ADE_MACOS_APPLICATIONS_DIR:-}}
if [[ -z "$applications" ]]; then
  # No location was named, so search both places install.sh can publish to. The
  # installer's default is ~/Applications and its documented machine-wide
  # opt-in is /Applications; checking only one of them exits without removing
  # anything and leaves the other copy's daemon running.
  candidates=()
  [[ -z "${HOME:-}" ]] || candidates+=("$HOME/Applications")
  candidates+=(/Applications)
  found=()
  for candidate in "${candidates[@]}"; do
    owned "$candidate" || continue
    candidate=$(cd "$candidate" && pwd -P)
    # ~/Applications can be a symlink to /Applications; the same physical
    # install must not be reported as two competing ones.
    [[ ${#found[@]} -eq 0 || "${found[0]}" != "$candidate" ]] || continue
    found+=("$candidate")
  done
  if (( ${#found[@]} == 0 )); then
    for candidate in "${candidates[@]}"; do
      echo "no Muxflow install found at $candidate/$name" >&2
    done
    exit 1
  fi
  if (( ${#found[@]} > 1 )); then
    echo "Muxflow is installed in more than one location:" >&2
    for candidate in "${found[@]}"; do
      echo "  $candidate/$name" >&2
    done
    echo "re-run naming the one to remove, e.g. uninstall.sh ${found[0]}" >&2
    exit 64
  fi
  applications=${found[0]}
fi

[[ "$applications" == /* && "$applications" != / && "$applications" != *$'\n'* ]] || {
  echo "unsafe applications directory" >&2; exit 64;
}
[[ -d "$applications" && ! -L "$applications" ]] || {
  echo "no Muxflow install found at $applications/$name" >&2; exit 1;
}
applications=$(cd "$applications" && pwd -P)
target="$applications/$name"
[[ -d "$target" && ! -L "$target" ]] || {
  echo "no Muxflow install found at $target" >&2; exit 1;
}
grep -Fxq "$owner" "$target/Contents/Resources/package-owner" 2>/dev/null || {
  echo "refusing to remove an application not owned by Muxflow: $target" >&2; exit 73;
}
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$target/Contents/Info.plist") == dev.muxflow.desktop ]] || {
  echo "refusing to remove an application not owned by Muxflow: $target" >&2; exit 73;
}

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
