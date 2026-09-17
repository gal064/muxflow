#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
work_root=${ADE_WORK_ROOT:-"$repo_root/tmp/work"}
cd "$repo_root"

clean_target() {
  local target_dir=$1
  [[ -d "$target_dir" ]] || return 0
  cargo clean --target-dir "$target_dir"
}

clean_target "$repo_root/target"
clean_target "$work_root/cache/release-target/macos"
clean_target "$work_root/cache/release-target/linux-x86_64"
clean_target "$work_root/cache/release-target/linux-aarch64"

# This includes the shared per-architecture targets and target trees left by
# the older source-digest-per-target layout. Only Cargo-generated directories
# beneath the compatible-helper cache are selected.
if [[ -d "$work_root/cache/compatible-host" ]]; then
  while IFS= read -r -d '' target_dir; do
    clean_target "$target_dir"
  done < <(find "$work_root/cache/compatible-host" -type d -name target -prune -print0)
fi

echo "RUST_CACHE_CLEAN development and release build artifacts removed"
