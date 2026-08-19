#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
target_arch=$(phase8_linux_target_arch)
docker_platform=$(phase8_docker_platform)
phase8_storage_begin "$repo_root" phase8-regressions
run_root=$PHASE8_EVIDENCE_DIR

cleanup() {
  local status=$?
  phase8_storage_finish "$status"
}
trap cleanup EXIT

# A lane that cannot run on this platform is recorded as an explicit skip with
# its reason, never as a pass. `results.tsv` therefore still shows the lane, and
# a reader can see the suite was not fully executed here.
skip_gate() {
  local name=$1
  local reason=$2
  printf '%s\tskip\t0\t%s\n' "$name" "$reason" >>"$run_root/results.tsv"
  printf 'SKIPPED %s: %s\n' "$name" "$reason" >&2
}

run_gate() {
  local name=$1
  shift
  local started=$SECONDS
  if "$@" > "$run_root/$name.log" 2>&1; then
    printf '%s\tpass\t%s\n' "$name" "$((SECONDS - started))" >> "$run_root/results.tsv"
  else
    printf '%s\tfail\t%s\n' "$name" "$((SECONDS - started))" >> "$run_root/results.tsv"
    tail -n 120 "$run_root/$name.log" >&2 || true
    return 1
  fi
}

: > "$run_root/results.tsv"
bookworm_helper="$PHASE8_WORK_DIR/tmux-ide-host-bookworm"
run_gate bookworm-helper release/linux/build-compatible-host.sh "$target_arch" "$bookworm_helper"
export ADE_TEST_BOOKWORM_HELPER="$bookworm_helper"
run_gate phase8-storage bash tests/release/test-storage.sh
run_gate rust-fmt cargo fmt --all -- --check
run_gate rust-clippy cargo clippy --workspace --all-targets -- -D warnings
run_gate rust-tests cargo test --workspace --all-targets
run_gate phase8-driver-fmt cargo fmt --manifest-path tests/release/protocol-driver/Cargo.toml -- --check
run_gate phase8-driver-clippy cargo clippy --manifest-path tests/release/protocol-driver/Cargo.toml -- -D warnings
run_gate phase8-driver-tests cargo test --manifest-path tests/release/protocol-driver/Cargo.toml
run_gate frontend-check pnpm check
run_gate frontend-tests pnpm test
run_gate frontend-build pnpm build
run_gate tauri-release pnpm tauri build --no-bundle
run_gate parser-fuzz cargo run --manifest-path tests/integration/fuzz-smoke/Cargo.toml -- 10000
run_gate phase0-local pnpm test:transport:local
run_gate phase0-ssh pnpm test:transport:ssh
run_gate phase1 pnpm test:lifecycle
run_gate phase2 pnpm test:protocol
run_gate phase3 pnpm test:shell
run_gate phase4 pnpm test:files
run_gate phase4-large bash tests/integration/filesystem/run-large-download.sh
run_gate phase5 pnpm test:git
run_gate phase5-ssh pnpm test:git:ssh
run_gate phase6 pnpm test:agents
run_gate phase6-ssh pnpm test:agents:ssh
run_gate phase7 pnpm test:transfers
run_gate phase7-parent pnpm test:transfers:parent-swap
run_gate phase7-transfer-smoke bash tests/integration/transfers/run-smoke.sh
if phase8_is_darwin; then
  # Builds the Linux desktop package, which links WebKitGTK and cannot be built
  # on macOS at any architecture. This lane is Linux-only by platform, so the
  # Phase 0-8 matrix is not fully executable on a Mac and must not be reported
  # as if it were.
  skip_gate phase8-package "Linux-only: builds a WebKitGTK desktop package"
else
  run_gate phase8-package pnpm test:package
fi
run_gate phase8-fault-security bash tests/release/run-fault-security.sh

# Bind the release evidence to the exact post-gate source set. Git's tracked
# plus non-ignored view excludes editor logs, incremental metadata, build
# outputs, and every nested tmp directory even in a new/uncommitted checkout.
git ls-files --cached --others --exclude-standard -z \
  | LC_ALL=C sort -z | xargs -0 sha256sum > "$run_root/source-tree-manifest.tsv"
sha256sum "$run_root/source-tree-manifest.tsv" | cut -d' ' -f1 \
  > "$run_root/source-tree-digest.txt"
phase8_storage_publish "$repo_root" phase8-regressions-latest
cat "$run_root/results.tsv"
