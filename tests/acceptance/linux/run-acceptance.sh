#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
phase8_storage_begin "$repo_root" phase9-acceptance
run_root=$PHASE8_EVIDENCE_DIR

cleanup() {
  local status=$?
  phase8_storage_finish "$status"
}
trap cleanup EXIT

run_gate() {
  local name=$1
  shift
  local started=$SECONDS
  if "$@" >"$run_root/$name.log" 2>&1; then
    printf '%s\tpass\t%s\n' "$name" "$((SECONDS - started))" >>"$run_root/results.tsv"
  else
    printf '%s\tfail\t%s\n' "$name" "$((SECONDS - started))" >>"$run_root/results.tsv"
    tail -n 120 "$run_root/$name.log" >&2 || true
    return 1
  fi
}

: >"$run_root/results.tsv"
run_gate source-scan bash tests/acceptance/linux/run-source-scan.sh
# This is the existing bounded 28-lane Phase 0–8 gate. It uses 16/64 MiB
# representative transfer lanes and never enables the exact 5 GiB release lane.
run_gate phase0-through-8-bounded bash tests/release/run-regressions.sh
regression_root=$(realpath "$repo_root/tmp/phase8-regressions-latest")
cp "$regression_root/results.tsv" "$run_root/phase8-regression-results.tsv"
cp "$regression_root/storage.json" "$run_root/phase8-regression-storage.json"

phase8_storage_publish "$repo_root" phase9-acceptance-latest
cat "$run_root/results.tsv"
