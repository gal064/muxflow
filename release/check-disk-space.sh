#!/usr/bin/env bash
set -euo pipefail

repo_root=${1:?repository root is required}
work_root=${2:?release work root is required}
minimum_free_kib=${ADE_RELEASE_MIN_FREE_KIB:-15728640}

directory_size_kib() {
  local path=$1
  if [[ -d "$path" ]]; then
    du -sk "$path" | awk '{print $1}'
  else
    printf '0\n'
  fi
}

mkdir -p "$work_root"
available_kib=$(df -Pk "$work_root" | awk 'NR == 2 {print $4}')
[[ "$available_kib" =~ ^[0-9]+$ ]] || {
  echo "RELEASE_DISK_BLOCKER: could not determine free space at $work_root" >&2
  exit 78
}

if ((available_kib < minimum_free_kib)); then
  available_gib=$((available_kib / 1024 / 1024))
  minimum_gib=$((minimum_free_kib / 1024 / 1024))
  development_kib=$(directory_size_kib "$repo_root/target")
  release_kib=$(directory_size_kib "$work_root/cache/release-target")
  helper_kib=$(directory_size_kib "$work_root/cache/compatible-host")
  echo "RELEASE_DISK_BLOCKER: package builds need at least ${minimum_gib} GiB free at $work_root; found ${available_gib} GiB" >&2
  echo "Rust cache usage (KiB): development=$development_kib release=$release_kib compatible-helpers=$helper_kib" >&2
  echo "Run 'pnpm clean:rust' to remove reproducible Rust build artifacts, then retry." >&2
  exit 78
fi
