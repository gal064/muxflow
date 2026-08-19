#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase7 backend/frontend deterministic gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$repo_root/tests/release/storage.sh"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
evidence="$repo_root/tmp/phase7-backend-$run_id"
mkdir -p "$evidence"
source "$repo_root/tests/integration/transfers/source-tree-evidence.sh"
phase7_capture_source_tree "$repo_root" "$evidence"
printf '%s\n' "$evidence" >"$repo_root/tmp/phase7-backend-latest"
cd "$repo_root"

cargo fmt --all -- --check >"$evidence/fmt.log" 2>&1 & fmt_pid=$!
cargo fmt --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml -- --check \
  >"$evidence/driver-fmt.log" 2>&1 & driver_fmt_pid=$!
cargo test -p tmux-agent-protocol \
  >"$evidence/protocol.log" 2>&1 & protocol_pid=$!
cargo test -p tmux-ide-host terminal_upload::tests -- --test-threads=1 \
  >"$evidence/host-upload.log" 2>&1 & host_pid=$!
cargo test -p tmux-ide-host terminal_upload::staging::tests \
  >"$evidence/host-staging-security.log" 2>&1 & host_staging_pid=$!
cargo test -p tmux-agent-desktop upload_manager::tests \
  >"$evidence/desktop-upload.log" 2>&1 & desktop_pid=$!
cargo test -p tmux-agent-desktop clipboard_staging::tests \
  >"$evidence/desktop-clipboard.log" 2>&1 & clipboard_pid=$!
cargo test -p tmux-agent-desktop local_destination::tests \
  >"$evidence/desktop-destination-security.log" 2>&1 & destination_pid=$!
cargo test -p tmux-agent-desktop transfer_event::tests \
  >"$evidence/typed-transfer-contract.log" 2>&1 & event_pid=$!
cargo test -p tmux-agent-desktop scheduler::tests -- --test-threads=1 \
  >"$evidence/canonical-engine.log" 2>&1 & engine_pid=$!
pnpm --dir apps/desktop test -- \
  src/features/terminal/terminalTransfers.test.ts \
  src/features/terminal/TerminalTransferSurface.test.tsx \
  src/features/terminal/terminalTransferApi.test.ts \
  >"$evidence/frontend-transfer.log" 2>&1 & frontend_pid=$!
cargo check --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml \
  >"$evidence/driver-check.log" 2>&1 & driver_pid=$!
bash tests/integration/transfers/package-scripts.sh \
  >"$evidence/package-scripts.log" 2>&1 & package_scripts_pid=$!

phase7_wait_all "$fmt_pid" "$driver_fmt_pid" "$protocol_pid" "$host_pid" "$host_staging_pid" \
  "$desktop_pid" "$clipboard_pid" "$destination_pid" "$event_pid" \
  "$engine_pid" "$frontend_pid" "$driver_pid" "$package_scripts_pid"
rg -Fq \
  'test service::filesystem::terminal_upload::tests::crash::sigkill_restart_matrix_converges_every_upload_transaction_boundary ...' \
  "$evidence/host-upload.log"
# `durable_journal_recovers_overwrite_and_removes_only_original_backup` is
# `#[cfg(target_os = "linux")]` in the product source and has been since the
# baseline, so this batch is 24 tests on Linux and 23 on macOS. Keep the count
# exact per platform rather than loosening the match, so a test that silently
# stops running is still caught.
if phase8_is_darwin; then
  expected_upload_tests=23
else
  expected_upload_tests=24
fi
rg -Fq "test result: ok. $expected_upload_tests passed; 0 failed" "$evidence/host-upload.log"
cargo clippy -p tmux-agent-protocol -p tmux-ide-host -p tmux-agent-desktop \
  --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 & clippy_pid=$!
cargo clippy --manifest-path tests/integration/transfers/protocol-driver/Cargo.toml -- -D warnings \
  >"$evidence/driver-clippy.log" 2>&1 & driver_clippy_pid=$!
phase7_wait_all "$clippy_pid" "$driver_clippy_pid"
phase7_assert_source_tree_unchanged "$repo_root" "$evidence"
jq -n --arg digest "$(<"$evidence/source-tree-digest.txt")" \
  '{
    passed: true,
    sourceTreeDigest: $digest,
    uploadCrashRecovery: {
      processTermination: "SIGKILL",
      freshProcessRecovery: true,
      transactionCases: 18,
      newAndOverwrite: true
    }
  }' >"$evidence/result.json"
printf '%s\n' "phase7 backend/frontend evidence: $evidence"
