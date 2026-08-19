#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
source "$repo_root/tests/release/storage.sh"
export ADE_EVIDENCE_ROOT="$repo_root/tmp/evidence/storage-selftest"
export ADE_PHASE8_MIN_FREE_KIB=1
phase8_storage_begin "$repo_root" storage-selftest
work=$PHASE8_WORK_DIR
evidence=$PHASE8_EVIDENCE_DIR
printf 'bounded\n' >"$work/disposable"
printf '{"status":"pass"}\n' >"$evidence/result.json"
phase8_storage_publish "$repo_root" phase8-storage-selftest-latest
phase8_storage_finish 0
[[ ! -e "$work" ]]
jq -e '.status == "pass" and .workRootDiskBacked and (.retainedEvidenceBytes < 1048576)' \
  "$evidence/storage.json" >/dev/null
rm -f "$repo_root/tmp/phase8-storage-selftest-latest"
rm -rf "$repo_root/tmp/evidence/storage-selftest"
printf 'phase8 storage policy: pass\n'
