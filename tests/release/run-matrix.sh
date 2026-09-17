#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
phase8_storage_begin "$repo_root" phase8-matrix
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
  if "$@" > "$run_root/$name.log" 2>&1; then
    printf '%s\tpass\t%s\n' "$name" "$((SECONDS - started))" >> "$run_root/results.tsv"
  else
    printf '%s\tfail\t%s\n' "$name" "$((SECONDS - started))" >> "$run_root/results.tsv"
    tail -n 100 "$run_root/$name.log" >&2 || true
    return 1
  fi
}

: > "$run_root/results.tsv"
exact_5gib=${ADE_PHASE8_EXACT_5GIB:-0}
if [[ "$exact_5gib" == 1 && "${ADE_PHASE8_EXACT_5GIB_CONFIRM:-}" != release ]]; then
  echo "exact 5 GiB matrix requires ADE_PHASE8_EXACT_5GIB_CONFIRM=release" >&2
  exit 64
fi
if [[ "$exact_5gib" == 1 ]]; then
  local_bytes=5368709120
  ssh_bytes=5368709120
  transfer_label=5gib
  matrix_mode=release-exact
else
  local_bytes=${ADE_PHASE7_LOCAL_BYTES:-16777216}
  ssh_bytes=${ADE_PHASE7_SSH_BYTES:-67108864}
  transfer_label=smoke
  matrix_mode=development-smoke
fi
jq -n --arg mode "$matrix_mode" --arg localBytes "$local_bytes" \
  --arg sshBytes "$ssh_bytes" \
  '{mode:$mode,localBytes:$localBytes,sshBytes:$sshBytes}' > "$run_root/matrix-mode.json"
run_gate fault-security bash tests/release/run-fault-security.sh
run_gate package bash tests/release/run-package.sh
run_gate scale-local bash tests/release/run-scale-local.sh
run_gate scale-ssh bash tests/release/run-scale-ssh.sh
run_gate terminal-flood-hidden-release bash tests/integration/protocol/run-backend.sh
run_gate "transfer-local-$transfer_label" env ADE_PHASE7_LOCAL_BYTES="$local_bytes" \
  bash tests/integration/transfers/run-local-transfer.sh
run_gate "transfer-ssh-$transfer_label" env ADE_PHASE7_SSH_BYTES="$ssh_bytes" \
  bash tests/integration/transfers/run-docker-ssh.sh

phase8_storage_publish "$repo_root" phase8-matrix-latest
cat "$run_root/results.tsv"
