#!/usr/bin/env bash
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
label="${1:-before}"
artifacts="$repo_root/tmp/phase14/$label"
if [[ -e "$artifacts" ]]; then
  printf '%s\n' "refusing to overwrite existing Phase 14 artifacts: $artifacts" >&2
  exit 2
fi
mkdir -p "$artifacts/logs" "$artifacts/phase12"

# Keep the opt-in measurement build reproducible and below common CI/worktree
# cache quotas. The packaged/wire lane gets its own cache because it builds a
# release helper while the deterministic fixtures use test artifacts.
export CARGO_TARGET_DIR="${ADE_PHASE14_CARGO_TARGET_DIR:-/tmp/muxflow-phase14-target}"
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_DEV_INCREMENTAL=false
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_PROFILE_TEST_INCREMENTAL=false
export UV_CACHE_DIR="${ADE_PHASE14_UV_CACHE_DIR:-/tmp/muxflow-phase14-uv-cache}"

fixture_status=0
uv run --no-project "$repo_root/tests/performance/optimization/collect-context.py" "$artifacts/context.json" || fixture_status=1
run_fixture() {
  local name="$1"
  shift
  "$@" >"$artifacts/logs/$name.log" 2>&1 || fixture_status=1
}

desktop_exec() {
  local tool="$1"
  shift
  if [[ -x "$repo_root/apps/desktop/node_modules/.bin/$tool" ]]; then
    (cd "$repo_root/apps/desktop" && "./node_modules/.bin/$tool" "$@")
  else
    pnpm --filter @tmux-agent-ide/desktop exec "$tool" "$@"
  fi
}

desktop_build() {
  desktop_exec tsc -b --pretty false && desktop_exec vite build
}

cd "$repo_root"
run_fixture terminal-frontend desktop_exec vitest run src/features/terminal/TerminalRenderer.test.ts src/perf/probe.test.ts
run_fixture explorer-wide desktop_exec vitest run src/features/files/ExplorerTree.test.tsx src/features/files/api.test.ts
run_fixture pane-resource cargo test -p tmux-control phase14_pane_resource_scaling_and_reveal_parity -- --ignored --nocapture
run_fixture desktop-frontend-build desktop_build
# This is a Rust unit fixture, not a bundle test. Avoid requiring or copying a
# target-specific sidecar while still giving generate_context! its real dist.
run_fixture transfer-admission env TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo test -p tmux-agent-desktop phase14_full_queue_reports_admission_and_exact_terminal_outcomes -- --ignored --nocapture
run_fixture ssh-master-coordination env TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo test -p tmux-agent-desktop phase14_fake_delayed_ssh_master_coordination_counts -- --ignored --nocapture --test-threads=1
run_fixture git-consumers cargo test -p tmux-ide-host phase14_thirty_two_consumers_report_native_watchers_and_status_processes -- --ignored --nocapture --test-threads=1
run_fixture git-processes cargo test -p tmux-ide-host phase14_warm_diff_and_mutation_process_counts -- --ignored --nocapture --test-threads=1

uv run --no-project "$repo_root/tests/performance/optimization/extract-metrics.py" "$artifacts/deterministic-metrics.json" "$artifacts"/logs/*.log || fixture_status=1

# The desktop test graph and release host graph can jointly exceed the 2 GiB
# project quota used by the isolated CI/worktree runner. Metrics are already
# durable at this point, so discard only the generated deterministic cache.
if [[ "${ADE_PHASE14_KEEP_TEST_TARGET:-0}" != "1" ]]; then
  cargo clean --target-dir "$CARGO_TARGET_DIR" >"$artifacts/logs/cargo-test-cache-clean.log" 2>&1 || fixture_status=1
fi

phase12_status=0
phase12_evidence_status=0
flood_seconds="${ADE_PHASE14_FLOOD_SECONDS:-10}"
phase12_runtime_file="$artifacts/phase12-runtime.txt"
CARGO_TARGET_DIR="${ADE_PHASE14_RELEASE_TARGET_DIR:-/tmp/muxflow-phase14-release}" \
  CARGO_PROFILE_RELEASE_DEBUG=0 \
  ADE_PHASE12_RUNTIME_FILE="$phase12_runtime_file" \
  ADE_PHASE12_LABEL="phase14-$label" ADE_PHASE12_FLOOD_SECONDS="$flood_seconds" \
  bash "$repo_root/tests/performance/runtime/run-perf.sh" >"$artifacts/logs/phase12-shaped-ssh.log" 2>&1
phase12_status=$?
if [[ -r "$phase12_runtime_file" ]]; then
  phase12_runtime="$(<"$phase12_runtime_file")"
  if [[ -d "$phase12_runtime" ]]; then
    # Keep only the small authoritative result/provenance set. Runtime sockets,
    # extracted helpers, and build trees are not evidence and can exceed the
    # worktree quota without adding anything to the verdict.
    phase12_evidence=(
      budget-report.txt docker-status.txt docker-unavailable.txt
      local-app.json local-raw.json docker-app.json docker-raw.json
      host-version.json docker-version.txt local-tmux-version.txt
      cargo-host-build.log cargo-driver-build.log cargo-remote-build.log
      local-runtime/diagnostics.json docker-runtime/diagnostics.json
    )
    for relative_path in "${phase12_evidence[@]}"; do
      if [[ -f "$phase12_runtime/$relative_path" ]]; then
        mkdir -p "$artifacts/phase12/$(dirname "$relative_path")"
        cp -a "$phase12_runtime/$relative_path" "$artifacts/phase12/$relative_path" || phase12_evidence_status=1
      fi
    done
  else
    phase12_evidence_status=1
  fi
else
  phase12_evidence_status=1
fi
uv run --no-project "$repo_root/tests/performance/optimization/validate-phase12.py" \
  "$artifacts/phase12" "phase14-$label" "$flood_seconds" || phase12_evidence_status=1
uv run --no-project "$repo_root/tests/performance/optimization/collect-context.py" \
  "$artifacts/context-end.json" "$artifacts/context.json" || fixture_status=1
uv run --no-project "$repo_root/tests/performance/optimization/summarize-baseline.py" \
  "$artifacts" "$fixture_status" "$phase12_status" "$phase12_evidence_status" "$flood_seconds"
summary_status=$?
printf '%s\n' "phase14 baseline artifacts: $artifacts"
exit "$summary_status"
