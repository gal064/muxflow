#!/usr/bin/env bash
# Release-tag routing must not silently upload iOS or classify stable tags as RCs.
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/../.." && pwd)

check() {
  local tag=$1 prerelease=$2 audience=$3 actual expected
  actual=$(bash "$repo_root/release/tag-info.sh" "$tag")
  expected=$(printf 'version=0.2.0\nprerelease=%s\nios_audience=%s' "$prerelease" "$audience")
  [[ "$actual" == "$expected" ]] || { printf 'Wrong routing for %s:\n%s\n' "$tag" "$actual" >&2; exit 1; }
}
check v0.2.0 false none
check v0.2.0-ios false external
check v0.2.0-ios-internal false internal
check v0.2.0-rc.1 true none
check v0.2.0-rc.1-ios true external
check v0.2.0-rc.1-ios-internal true internal

for tag in 0.2.0 v0.2 v0.2.0-beta.1 v0.2.0-internal v0.2.0-ios-external \
  v0.2.0-ios-rc.1 v0.2.0-ios-internal-extra v0.2.0-rc. v0.2.0-rc.1-ios-ios \
  v0.2.0-ios/extra 'v0.2.0-ios internal'; do
  if bash "$repo_root/release/tag-info.sh" "$tag" >/dev/null 2>&1; then
    printf 'Accepted unsupported tag: %s\n' "$tag" >&2
    exit 1
  fi
done
printf 'Release tag routing: OK\n'
