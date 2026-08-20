#!/usr/bin/env bash
set -euo pipefail

source_app=${1:-}
applications=${ADE_MACOS_APPLICATIONS_DIR:-"$HOME/Applications"}
name='Muxflow.app'
owner='dev.muxflow.desktop:1'

[[ "$source_app" == /* && -d "$source_app" && ! -L "$source_app" ]] || {
  echo "usage: install.sh /absolute/path/to/Muxflow.app" >&2; exit 64;
}
[[ "$applications" == /* && "$applications" != / && "$applications" != *$'\n'* ]] || {
  echo "unsafe applications directory" >&2; exit 64;
}
mkdir -p "$applications"
[[ -d "$applications" && ! -L "$applications" ]]
applications=$(cd "$applications" && pwd -P)
target="$applications/$name"

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
published=false
cleanup() {
  if $published; then
    rm -rf "$target"
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
[[ ! -e "$target" ]] || mv "$target" "$backup"
mv "$stage" "$target"
published=true
if [[ ${ADE_PHASE10_TEST_FAIL_AFTER_PUBLICATION:-0} == 1 ]]; then
  echo "injected macOS package publication failure" >&2
  false
fi
"$(cd "$(dirname "$0")" && pwd -P)/verify-package.sh" "$target"
published=false
rm -rf "$backup" "$transaction"
trap - EXIT
printf 'Installed %s; tmux sessions and user configuration were preserved.\n' "$target"
