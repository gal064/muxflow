#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
phase8_storage_begin "$repo_root" phase8-scale-local
run_root=$PHASE8_WORK_DIR
evidence=$PHASE8_EVIDENCE_DIR
fixture_home="$run_root/home"
fixture_repo="$run_root/repository"
runtime="$run_root/runtime"
prefix="$run_root/prefix"
socket="phase8-scale-$$"
mkdir -p "$fixture_home" "$runtime"

cleanup() {
  local status=$?
  # Stop whichever helper this run actually used: the packaged macOS binary on
  # Darwin, or the one installed from the Linux archive elsewhere.
  local stop_helper="${helper:-$prefix/lib/tmux-agent-ide/tmux-ide-host}"
  [[ ! -x "$stop_helper" ]] || \
    HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" \
      "$stop_helper" daemon-stop >/dev/null 2>&1 || true
  env -u TMUX tmux -L "$socket" kill-server >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

tests/release/create-scale-fixture.sh "$fixture_repo" "$socket" > "$run_root/fixture.log"

# The Linux release archive and its install.sh contain ELF binaries and cannot
# be installed or executed on macOS, so the packaged-install half of this lane is
# Linux-only. The scale measurement itself is not: run it against the packaged
# macOS helper from the candidate bundle, which is the artifact that actually
# ships on this platform. The macOS packaged install/upgrade/uninstall path has
# its own coverage in tests/acceptance/macos/run-package-lifecycle.sh.
if phase8_is_darwin; then
  helper="$repo_root/target/release/bundle/macos/tmux Agent IDE.app/Contents/MacOS/tmux-ide-host"
  if [[ ! -x "$helper" ]]; then
    echo "macOS scale gate needs the packaged candidate; run release/macos/build-package.sh first" >&2
    exit 69
  fi
  printf 'packaged macOS helper: %s\n' "$helper" > "$run_root/package-verify.log"
  shasum -a 256 "$helper" >> "$run_root/package-verify.log"
else
  helper=''
fi

archive=${ADE_PHASE8_PACKAGE_ARCHIVE:-}
if [[ -n "$helper" ]]; then archive=skip-linux-package; fi
if [[ -z "$archive" && -L "$repo_root/tmp/phase8-package-latest" ]]; then
  package_run=$(realpath "$repo_root/tmp/phase8-package-latest")
  archive=$(find "$package_run/output-a" -maxdepth 1 -name '*-linux-x86_64.tar.gz' -print -quit)
fi
if [[ -z "$archive" ]]; then
  ADE_RELEASE_OUTPUT_DIR="$run_root/release" release/linux/build-package.sh x86_64 > "$run_root/package.log"
  archive=$(tail -n 1 "$run_root/package.log")
fi
if [[ -z "$helper" ]]; then
  release/linux/verify-package.sh "$archive" > "$run_root/package-verify.log"
  mkdir -p "$run_root/unpack"
  tar -xzf "$archive" -C "$run_root/unpack"
  package_root=$(find "$run_root/unpack" -mindepth 1 -maxdepth 1 -type d -print -quit)
  HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" "$package_root/install.sh" > "$run_root/install.log"
  helper="$prefix/lib/tmux-agent-ide/tmux-ide-host"
fi
cargo build --manifest-path tests/release/protocol-driver/Cargo.toml > "$run_root/driver-build.log" 2>&1
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$CARGO_TARGET_DIR/debug/release-test-driver" local "$helper" \
  > "$run_root/result.json" 2> "$run_root/driver.log"
jq -e '.status == "pass" and .sessions >= 20 and .windows >= 100 and .panes >= 100 and .rootEntries >= 250' \
  "$run_root/result.json" >/dev/null
if [[ -x "$prefix/lib/tmux-agent-ide/uninstall.sh" ]]; then
  HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" ADE_INSTALL_PREFIX="$prefix" \
    "$prefix/lib/tmux-agent-ide/uninstall.sh" > "$run_root/uninstall.log"
fi
for artifact in result.json fixture.log package-verify.log driver.log driver-build.log install.log uninstall.log; do
  [[ ! -f "$run_root/$artifact" ]] || cp "$run_root/$artifact" "$evidence/$artifact"
done
phase8_storage_publish "$repo_root" phase8-scale-local-latest
cat "$run_root/result.json"
