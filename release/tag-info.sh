#!/usr/bin/env bash
# Parse the release tag. iOS uploads are opt-in; only RC tags are prereleases.
# The iOS suffix only switches the upload on; it is never part of the release name.
set -euo pipefail

tag=${1:?usage: tag-info.sh vX.Y.Z[-rc.N][-ios[-internal]]}
if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)(-rc\.[0-9]+)?(-ios(-internal)?)?$ ]]; then
  printf 'Unsupported release tag: %s\n' "$tag" >&2
  exit 1
fi
version=${BASH_REMATCH[1]}
name=$version${BASH_REMATCH[2]}
prerelease=false
[[ -z "${BASH_REMATCH[2]}" ]] || prerelease=true
audience=none
if [[ -n "${BASH_REMATCH[3]}" ]]; then
  audience=external
  [[ -z "${BASH_REMATCH[4]}" ]] || audience=internal
fi
printf 'version=%s\nname=%s\nprerelease=%s\nios_audience=%s\n' "$version" "$name" "$prerelease" "$audience"
