#!/usr/bin/env bash
# Builds the package and installs it into ~/.local (or ADE_INSTALL_PREFIX), the
# Linux counterpart of release/macos/build-and-install.sh. It skips
# verify-package.sh, whose glibc baseline check refuses a desktop built on a
# distribution newer than the release runner; the installer still checks the
# package's own checksums. Needs Docker for the helper build (see
# build-package.sh).
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
archive=$("$repo_root/release/linux/build-package.sh" "$@" | tail -n 1)

work_root=${ADE_WORK_ROOT:-"$repo_root/tmp/work"}
unpack=$(mktemp -d "$work_root/package-builds/install.XXXXXX")
trap 'rm -rf "$unpack"' EXIT
tar -xzf "$archive" -C "$unpack"
"$unpack"/*/install.sh
