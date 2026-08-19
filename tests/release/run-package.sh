#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
phase8_storage_begin "$repo_root" phase8-package
run_root=$PHASE8_WORK_DIR
evidence=$PHASE8_EVIDENCE_DIR
fixture_home="$run_root/home"
prefix="$run_root/prefix"
output_a="$run_root/output-a"
output_b="$run_root/output-b"
runtime="$run_root/runtime"
socket="phase8-package-$$"
architecture=${ADE_PHASE8_PACKAGE_ARCH:-x86_64}
mkdir -p "$fixture_home" "$output_a" "$output_b" "$runtime"
chmod 0700 "$runtime"

cleanup() {
  local status=$?
  env -u TMUX tmux -L "$socket" kill-server >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

clean_target_a="$run_root/clean-target-a"
clean_target_b="$run_root/clean-target-b"
source_b="$run_root/source-b"
SOURCE_DATE_EPOCH=1704067200 CARGO_TARGET_DIR="$clean_target_a" ADE_RELEASE_OUTPUT_DIR="$output_a" release/linux/build-package.sh "$architecture" > "$evidence/build-a.out"
rm -rf "$clean_target_a"
mkdir -p "$source_b"
# Build checkout B from the same tracked/non-ignored source set used by the
# final source digest. This excludes nested test tmp, editor residue, and every
# build product even in a new all-untracked development checkout.
git ls-files --cached --others --exclude-standard -z \
  | tar --null --verbatim-files-from -T - -cf - | tar -xf - -C "$source_b"
ln -s "$repo_root/node_modules" "$source_b/node_modules"
ln -s "$repo_root/apps/desktop/node_modules" "$source_b/apps/desktop/node_modules"
mkdir -p "$source_b/tmp"
(
  cd "$source_b"
  SOURCE_DATE_EPOCH=1704067200 CARGO_TARGET_DIR="$clean_target_b" \
    ADE_RELEASE_OUTPUT_DIR="$output_b" release/linux/build-package.sh "$architecture"
) > "$evidence/build-b.out"
rm -rf "$clean_target_b"
archive_a=$(tail -n 1 "$evidence/build-a.out")
archive_b=$(tail -n 1 "$evidence/build-b.out")
release/linux/verify-package.sh "$archive_a" > "$evidence/verify.log"
digest_a=$(sha256sum "$archive_a" | cut -d' ' -f1)
digest_b=$(sha256sum "$archive_b" | cut -d' ' -f1)
printf 'a\t%s\nb\t%s\n' "$digest_a" "$digest_b" > "$evidence/reproducibility-digests.tsv"
if [[ "$digest_a" != "$digest_b" ]]; then
  compare_a="$run_root/compare-a"
  compare_b="$run_root/compare-b"
  mkdir -p "$compare_a" "$compare_b"
  tar -xzf "$archive_a" -C "$compare_a"
  tar -xzf "$archive_b" -C "$compare_b"
  root_a=$(find "$compare_a" -mindepth 1 -maxdepth 1 -type d -print -quit)
  root_b=$(find "$compare_b" -mindepth 1 -maxdepth 1 -type d -print -quit)
  diff -u "$root_a/SHA256SUMS" "$root_b/SHA256SUMS" \
    > "$evidence/reproducibility-content.diff" || true
  for binary in tmux-agent-desktop tmux-ide-host tmux-ide-host-x86_64; do
    printf '%s\ta\t%s\n' "$binary" "$(sha256sum "$root_a/bin/$binary" | cut -d' ' -f1)"
    printf '%s\tb\t%s\n' "$binary" "$(sha256sum "$root_b/bin/$binary" | cut -d' ' -f1)"
  done > "$evidence/reproducibility-binaries.tsv"
  echo "independent package archives are not reproducible" >&2
  exit 1
fi

unpack="$run_root/unpack"
mkdir -p "$unpack"
tar -xzf "$archive_a" -C "$unpack"
package_root=$(find "$unpack" -mindepth 1 -maxdepth 1 -type d -print -quit)
if strings "$package_root/bin/tmux-agent-desktop" | grep -F "$repo_root" >/dev/null \
  || strings "$package_root/bin/tmux-agent-desktop" | grep -F "$source_b" >/dev/null \
  || strings "$package_root/bin/tmux-agent-desktop" | grep -F "$clean_target_a" >/dev/null \
  || strings "$package_root/bin/tmux-agent-desktop" | grep -F "$clean_target_b" >/dev/null; then
  echo "release desktop embeds a private checkout or target path" >&2
  exit 1
fi
# Retain the independently-built archives and logs, not multi-gigabyte Cargo
# intermediates. A rerun must rebuild both roots from clean locations.
rm -rf "$source_b"
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$package_root/bin:/artifact:ro" debian:bookworm-slim \
  /artifact/tmux-ide-host version > "$evidence/bookworm-version.json"

# Every install ancestor is fail-closed. A pre-existing share symlink must not
# redirect desktop assets outside the chosen prefix.
escape_prefix="$run_root/escape-prefix"
escape_target="$run_root/escape-target"
mkdir -p "$escape_prefix/lib" "$escape_prefix/bin" "$escape_target"
ln -s "$escape_target" "$escape_prefix/share"
if HOME="$fixture_home" ADE_INSTALL_PREFIX="$escape_prefix" "$package_root/install.sh" \
  > "$evidence/symlink-install.out" 2> "$evidence/symlink-install.err"; then
  echo "installer followed a symlinked destination ancestor" >&2
  exit 1
fi
[[ -z "$(find "$escape_target" -mindepth 1 -print -quit)" ]]

HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" "$package_root/install.sh" > "$evidence/install.log"
[[ -x "$prefix/lib/tmux-agent-ide/tmux-agent-desktop" ]]
[[ -x "$prefix/lib/tmux-agent-ide/tmux-ide-host" ]]
[[ -x "$prefix/lib/tmux-agent-ide/tmux-ide-host-$architecture" ]]
[[ -L "$prefix/bin/tmux-ide-host" ]]
[[ -f "$prefix/share/applications/tmux-agent-ide.desktop" ]]
[[ -f "$prefix/share/icons/hicolor/256x256/apps/tmux-agent-ide.png" ]]
HOME="$fixture_home" "$prefix/bin/tmux-ide-host" version > "$evidence/version.json"
HOME="$fixture_home" "$prefix/bin/tmux-ide-host" doctor > "$evidence/doctor.json"

mkdir -p "$fixture_home/.config/tmux-agent-ide"
printf 'preserve-me\n' > "$fixture_home/.config/tmux-agent-ide/user-state"
env -u TMUX tmux -L "$socket" new-session -d -s survives-uninstall

# An in-place second install is the upgrade path and must preserve user state.
HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" "$package_root/install.sh" > "$evidence/upgrade.log"
grep -Fxq preserve-me "$fixture_home/.config/tmux-agent-ide/user-state"
printf 'rollback sentinel\n' > "$prefix/lib/tmux-agent-ide/rollback-sentinel"
if HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" ADE_PHASE8_TEST_FAIL_AFTER_LIB_PUBLICATION=1 \
  "$package_root/install.sh" > "$evidence/injected-upgrade.out" 2> "$evidence/injected-upgrade.err"; then
  echo "injected upgrade unexpectedly succeeded" >&2
  exit 1
fi
grep -Fxq 'rollback sentinel' "$prefix/lib/tmux-agent-ide/rollback-sentinel"
[[ -L "$prefix/bin/tmux-agent-ide" && -f "$prefix/share/applications/tmux-agent-ide.desktop" ]]
rm "$prefix/lib/tmux-agent-ide/rollback-sentinel"

# The same confinement applies to uninstall, even after a hostile ancestor
# replacement. It must not touch the symlink target or partially uninstall.
mv "$prefix/share" "$run_root/real-share"
mkdir "$run_root/uninstall-escape"
printf 'outside sentinel\n' > "$run_root/uninstall-escape/sentinel"
ln -s "$run_root/uninstall-escape" "$prefix/share"
if HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" "$prefix/lib/tmux-agent-ide/uninstall.sh" \
  > "$evidence/symlink-uninstall.out" 2> "$evidence/symlink-uninstall.err"; then
  echo "uninstaller followed a symlinked destination ancestor" >&2
  exit 1
fi
grep -Fxq 'outside sentinel' "$run_root/uninstall-escape/sentinel"
[[ -x "$prefix/lib/tmux-agent-ide/tmux-agent-desktop" ]]
rm "$prefix/share"
mv "$run_root/real-share" "$prefix/share"

# The packaged uninstaller must use the host's canonical ownership parser for
# both current Codex and legacy Claude hook formats and fail closed.
mkdir -p "$fixture_home/.codex" "$fixture_home/.claude"
printf '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"%s hook ingest --adapter codex --managed-owner tmux-agent-ide --managed-version 2"}]}]}}\n' \
  "'$prefix/lib/tmux-agent-ide/tmux-ide-host'" > "$fixture_home/.codex/hooks.json"
printf '%s\n' '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"tmux-ide-host hook ingest --adapter claude-code # tmux-agent-ide-managed:v1"}]}]}}' \
  > "$fixture_home/.claude/settings.json"
if HOME="$fixture_home" ADE_INSTALL_PREFIX="$prefix" "$prefix/lib/tmux-agent-ide/uninstall.sh" \
  > "$evidence/managed-hook-uninstall.out" 2> "$evidence/managed-hook-uninstall.err"; then
  echo "uninstall unexpectedly removed a helper referenced by managed hooks" >&2
  exit 1
fi
[[ -x "$prefix/lib/tmux-agent-ide/tmux-ide-host" ]]
rm -f "$fixture_home/.codex/hooks.json" "$fixture_home/.claude/settings.json"

HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$prefix/lib/tmux-agent-ide/tmux-ide-host" daemon \
  > "$evidence/daemon.log" 2>&1 &
daemon_launcher=$!
for _ in $(seq 1 100); do [[ -S "$runtime/host.sock" ]] && break; sleep 0.02; done
[[ -S "$runtime/host.sock" ]]
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" ADE_INSTALL_PREFIX="$prefix" \
  "$prefix/lib/tmux-agent-ide/uninstall.sh" > "$evidence/uninstall.log"
wait "$daemon_launcher"
[[ ! -e "$prefix/bin/tmux-agent-ide" ]]
[[ ! -e "$prefix/lib/tmux-agent-ide" ]]
[[ ! -e "$prefix" ]]
grep -Fxq preserve-me "$fixture_home/.config/tmux-agent-ide/user-state"
env -u TMUX tmux -L "$socket" has-session -t survives-uninstall

mkdir -p "$evidence/output-a"
retained_archive="$evidence/output-a/$(basename "$archive_a")"
cp "$archive_a" "$retained_archive"
cp "$archive_a.sha256" "$retained_archive.sha256"
jq -n \
  --arg archive "$retained_archive" \
  --arg digest "$digest_a" \
  --arg architecture "$architecture" \
  '{status:"pass", architecture:$architecture, archive:$archive, reproducibleDigest:$digest, independentCheckoutAndTarget:true, debian12PackagedHelper:true, ancestorSymlinkConfinement:true, cleanInstall:true, upgradePreservedConfig:true, uninstallPreservedConfigAndTmux:true}' \
  > "$evidence/result.json"
phase8_storage_publish "$repo_root" phase8-package-latest
cat "$evidence/result.json"
