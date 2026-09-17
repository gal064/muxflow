#!/usr/bin/env bash
#
# Phase 13.6.4: the installer, against a copy of this machine's real agent
# configuration.
#
# The committed fixture is a hand-written copy of that shape and is asserted in
# a unit test. This lane runs the same installer against the actual file, so a
# key, an ordering or a matcher the fixture got wrong cannot hide.
#
# The real file is **copied and never written**. Everything happens under a
# scratch home; `--settings-path` points the installer at the copy, and the
# original is compared byte-for-byte at the end to prove it was not touched.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
helper=${ADE_HELPER:-"$here/../../target/debug/muxflow-host"}
source_config=${ADE_REAL_SETTINGS:-"$HOME/.claude/settings.json"}
work=$(mktemp -d "${TMPDIR:-/tmp}/ade13-installer.XXXXXX")
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
[[ -x "$helper" ]] || fail "helper not built at $helper"
[[ -f "$source_config" ]] || fail "no configuration to copy at $source_config"

original_digest=$(shasum -a 256 "$source_config" 2>/dev/null | cut -d' ' -f1 \
  || sha256sum "$source_config" | cut -d' ' -f1)
mkdir -p "$work/home/.claude"
cp "$source_config" "$work/home/.claude/settings.json"
copy="$work/home/.claude/settings.json"
cp "$copy" "$work/before.json"
echo "copied $(wc -c < "$copy" | tr -d ' ') bytes of real configuration"

run() {
  "$helper" hook "$1" --adapter claude-code --home "$work/home" --settings-path "$copy"
}

before=$(run status)
echo "$before" | grep -Eq '"wiring":"(notWired|partial)"' \
  || fail "expected the real configuration to need installation or upgrade, got $before"

installed=$(run install)
echo "$installed" | grep -q '"changed":true' || fail "install reported no change"
echo "$installed" | grep -q '"wiring":"wired"' || fail "install did not wire it: $installed"

repeat=$(run install)
echo "$repeat" | grep -q '"changed":false' || fail "a second install was not idempotent: $repeat"

# Every command that was there is still there, byte for byte. Read a line at a
# time rather than through word splitting: these are real shell commands with
# spaces, quotes and brackets in them, and `for x in $(...)` tears them into
# fragments — one of which, on a real configuration, was `-r`.
missing=0
while IFS= read -r command_line; do
  [[ -n "$command_line" ]] || continue
  [[ "$command_line" == *"muxflow-host"*"managed-owner muxflow"* ]] && continue
  grep -qF -- "$command_line" "$copy" || { echo "lost: $command_line" >&2; missing=1; }
done < <(grep -o '"command": *"[^"]*"' "$work/before.json" | sort -u)
[[ "$missing" == "0" ]] || fail "install lost or altered a pre-existing hook command"
echo "every pre-existing hook command survived the install"

# The backup is the original, byte for byte — this is what a user would restore
# from, so it is the copy that has to be exact.
backup="$copy.muxflow.backup"
[[ -f "$backup" ]] || fail "no backup was written"
cmp -s "$work/before.json" "$backup" || fail "the backup is not the original file"
echo "backup is byte-identical to the original"

removed=$(run uninstall)
echo "$removed" | grep -q '"wiring":"notWired"' || fail "uninstall left it wired: $removed"
! grep -qF 'muxflow' "$copy" || fail "a managed entry survived the uninstall"
while IFS= read -r command_line; do
  [[ -n "$command_line" ]] || continue
  [[ "$command_line" == *"muxflow-host"*"managed-owner muxflow"* ]] && continue
  grep -qF -- "$command_line" "$copy" || fail "uninstall lost $command_line"
done < <(grep -o '"command": *"[^"]*"' "$work/before.json" | sort -u)
# Whitespace and key order are not compared here: the installer rewrites the
# file through a JSON serializer, so a re-indent is expected and harmless.
# `install_merges_into_a_real_settings_shape_without_disturbing_its_owner`
# asserts the *parsed* round trip against the committed fixture.
echo "uninstall left every foreign command and no managed one"

current_digest=$(shasum -a 256 "$source_config" 2>/dev/null | cut -d' ' -f1 \
  || sha256sum "$source_config" | cut -d' ' -f1)
[[ "$original_digest" == "$current_digest" ]] \
  || fail "the real configuration at $source_config was modified"
echo "the real $source_config is unchanged ($current_digest)"

echo "PASS: installer against this machine's real configuration shape"
