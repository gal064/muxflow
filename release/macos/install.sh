#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
work_root=${ADE_WORK_ROOT:-"$repo/tmp/work"}
release_target=${CARGO_TARGET_DIR:-"$work_root/cache/release-target/macos"}
[[ "$release_target" == /* ]] || release_target="$repo/$release_target"
source_app=${1:-"$release_target/release/bundle/macos/Muxflow.app"}
# The machine-wide /Applications is the default: it is where Spotlight, the
# Dock and Launchpad look first, so a per-user copy alongside it is how stale
# versions keep getting launched. The account must be able to write there (any
# admin account can); a rootless per-user install remains available as an
# explicit ADE_MACOS_APPLICATIONS_DIR="$HOME/Applications" opt-in.
applications=${ADE_MACOS_APPLICATIONS_DIR:-/Applications}
name='Muxflow.app'
owner='dev.muxflow.desktop:1'

[[ "$source_app" == /* && -d "$source_app" && ! -L "$source_app" ]] || {
  echo "usage: install.sh [/absolute/path/to/Muxflow.app]" >&2
  echo "default package not found at $source_app" >&2
  exit 64
}
[[ "$applications" == /* && "$applications" != / && "$applications" != *$'\n'* ]] || {
  echo "unsafe applications directory" >&2; exit 64;
}
mkdir -p "$applications"
[[ -d "$applications" && ! -L "$applications" ]]
applications=$(cd "$applications" && pwd -P)
target="$applications/$name"

# Installing into one well-known location leaves any copy in the other one
# behind, still running its own daemon. Migrating it automatically would be a
# destructive guess, so name both paths and let the operator decide.
alternates=(/Applications)
[[ -z "${HOME:-}" ]] || alternates+=("$HOME/Applications")
for alternate in "${alternates[@]}"; do
  [[ -d "$alternate/$name" && ! -L "$alternate/$name" ]] || continue
  grep -Fxq "$owner" "$alternate/$name/Contents/Resources/package-owner" 2>/dev/null || continue
  alternate=$(cd "$alternate" && pwd -P)
  [[ "$alternate" != "$applications" ]] || continue
  echo "warning: Muxflow is also installed at $alternate/$name" >&2
  echo "warning: this run installs to $target; remove the other copy with: release/macos/uninstall.sh $alternate" >&2
done

"$(cd "$(dirname "$0")" && pwd -P)/verify-package.sh" "$source_app"
if [[ -e "$target" || -L "$target" ]]; then
  [[ -d "$target" && ! -L "$target" ]]
  grep -Fxq "$owner" "$target/Contents/Resources/package-owner" || {
    echo "refusing to replace an application not owned by Muxflow" >&2; exit 73;
  }
fi

umask 077
transaction=$(mktemp -d "$applications/.muxflow.install.XXXXXX")
stage="$transaction/new.app"
backup="$transaction/old.app"
staged=false
committed=false
# The restore is keyed off the backup directory, not off a "we already
# published" flag: between moving the old app aside and moving the new one into
# place there is a window where a failed or interrupted `mv` leaves the target
# missing, and a flag-driven cleanup would delete the only surviving copy of the
# previous install along with the transaction directory. The staged/stage test
# is read back from the filesystem so an interrupt between a successful `mv` and
# the next assignment still rolls the publication back.
cleanup() {
  if ! $committed; then
    if [[ -d "$backup" ]] || { $staged && [[ ! -e "$stage" ]]; }; then
      rm -rf "$target"
    fi
    [[ ! -d "$backup" ]] || mv "$backup" "$target"
  fi
  rm -rf "$transaction"
}
trap cleanup EXIT

# Preserve Gatekeeper quarantine and every other extended attribute from the
# source. This package is intentionally ad-hoc signed for internal use; the
# installer must never suppress the operating system's trust signal.
ditto --extattr "$source_app" "$stage"
"$(cd "$(dirname "$0")" && pwd -P)/verify-package.sh" "$stage"
staged=true
[[ ! -e "$target" ]] || mv "$target" "$backup"
if [[ ${ADE_PHASE10_TEST_FAIL_BEFORE_PUBLICATION:-0} == 1 ]]; then
  echo "injected macOS package publication failure" >&2
  false
fi
mv "$stage" "$target"
if [[ ${ADE_PHASE10_TEST_FAIL_AFTER_PUBLICATION:-0} == 1 ]]; then
  echo "injected macOS package publication failure" >&2
  false
fi
"$(cd "$(dirname "$0")" && pwd -P)/verify-package.sh" "$target"
committed=true
rm -rf "$backup" "$transaction"
trap - EXIT
printf 'Installed %s; tmux sessions and user configuration were preserved.\n' "$target"
