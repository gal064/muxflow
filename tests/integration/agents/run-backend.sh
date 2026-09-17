#!/usr/bin/env bash
set -euo pipefail
trap 'echo "phase6 backend gate failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
evidence="$repo_root/tmp/phase6-backend-$run_id"
mkdir -p "$evidence"
printf '%s\n' "$evidence" >"$repo_root/tmp/phase6-backend-latest"
cd "$repo_root"

cargo fmt --all -- --check >"$evidence/fmt.log" 2>&1 & fmt_pid=$!
cargo test -p tmux-agent-protocol --test roundtrip >"$evidence/protocol.log" 2>&1 & protocol_pid=$!
cargo test -p tmux-control >"$evidence/tmux-control.log" 2>&1 & tmux_pid=$!
cargo test -p muxflow-host --bin muxflow-host -- --test-threads=1 >"$evidence/host.log" 2>&1 & host_pid=$!
cargo test -p muxflow connection::agent >"$evidence/desktop-bridge.log" 2>&1 & desktop_pid=$!

parallel_failed=0
for pid in "$fmt_pid" "$protocol_pid" "$tmux_pid" "$host_pid" "$desktop_pid"; do
  if ! wait "$pid"; then
    parallel_failed=1
  fi
done
((parallel_failed == 0))
# hook_cli launches the built helper repeatedly. Run it after the host test
# build so Cargo cannot replace that executable between a daemon launch and a
# hook launch, which is now correctly detected as an exact-build mismatch.
cargo test -p muxflow-host --test hook_cli >"$evidence/hook-cli.log" 2>&1
cargo clippy -p tmux-control -p muxflow-host -p tmux-agent-protocol -p muxflow \
  --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 & clippy_pid=$!
wait "$clippy_pid"
printf '%s\n' "phase6 backend evidence: $evidence"
