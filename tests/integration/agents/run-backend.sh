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
cargo test -p tmux-ide-host --bin tmux-ide-host -- --test-threads=1 >"$evidence/host.log" 2>&1 & host_pid=$!
cargo test -p tmux-ide-host --test hook_cli >"$evidence/hook-cli.log" 2>&1 & cli_pid=$!
cargo test -p tmux-agent-desktop connection::agent >"$evidence/desktop-bridge.log" 2>&1 & desktop_pid=$!

wait "$fmt_pid" "$protocol_pid" "$tmux_pid" "$host_pid" "$cli_pid" "$desktop_pid"
cargo clippy -p tmux-control -p tmux-ide-host -p tmux-agent-protocol -p tmux-agent-desktop \
  --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 & clippy_pid=$!
wait "$clippy_pid"
printf '%s\n' "phase6 backend evidence: $evidence"
