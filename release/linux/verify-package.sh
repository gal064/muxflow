#!/usr/bin/env bash
set -euo pipefail

archive=${1:?usage: verify-package.sh ARCHIVE}
expected=${archive}.sha256
[[ -f "$archive" && -f "$expected" ]]
read -r expected_digest expected_name extra < "$expected"
[[ "$expected_digest" =~ ^[0-9a-f]{64}$ && "$expected_name" == "$(basename "$archive")" && -z "${extra:-}" ]]
[[ "$(sha256sum "$archive" | cut -d' ' -f1)" == "$expected_digest" ]]

top_level=""
while IFS= read -r entry; do
  [[ -n "$entry" && "$entry" != /* && "$entry" != *$'\n'* && "$entry" != *$'\r'* ]]
  case "/$entry/" in */../*|*/./*) exit 1 ;; esac
  current=${entry%%/*}
  [[ -n "$current" ]]
  if [[ -z "$top_level" ]]; then top_level=$current; else [[ "$current" == "$top_level" ]]; fi
done < <(tar -tzf "$archive")
[[ -n "$top_level" ]]

verify_root=$(mktemp -d)
trap 'rm -rf "$verify_root"' EXIT
tar --no-same-owner --no-same-permissions -xzf "$archive" -C "$verify_root"
package_root=$(find "$verify_root" -mindepth 1 -maxdepth 1 -type d -print -quit)
[[ -n "$package_root" ]]
[[ -z "$(find "$verify_root" -mindepth 1 ! -type d ! -type f -print -quit)" ]]
(
  cd "$package_root"
  sha256sum -c SHA256SUMS
  find . -type f ! -name SHA256SUMS -print | LC_ALL=C sort > "$verify_root/actual-files.txt"
  sed -n 's/^[0-9a-f]\{64\}  //p' SHA256SUMS | LC_ALL=C sort > "$verify_root/manifest-files.txt"
  diff -u "$verify_root/manifest-files.txt" "$verify_root/actual-files.txt"
)
case "$(basename "$archive")" in
  *-linux-x86_64.tar.gz) expected_machine='Advanced Micro Devices X86-64' ;;
  *-linux-aarch64.tar.gz) expected_machine='AArch64' ;;
  *) echo "archive name does not declare a supported architecture" >&2; exit 1 ;;
esac
for binary in "$package_root/bin/tmux-agent-desktop" "$package_root/bin/tmux-ide-host"; do
  readelf -h "$binary" | grep -F "Machine:                           $expected_machine" >/dev/null
done
for helper in "$package_root"/bin/tmux-ide-host-x86_64 "$package_root"/bin/tmux-ide-host-aarch64; do
  [[ -f "$helper" ]] || continue
  case "$helper" in
    *-x86_64) helper_machine='Advanced Micro Devices X86-64' ;;
    *-aarch64) helper_machine='AArch64' ;;
  esac
  readelf -h "$helper" | grep -F "Machine:                           $helper_machine" >/dev/null
done
strings "$package_root/bin/tmux-agent-desktop" | grep -F "default-src 'self' customprotocol: asset:" >/dev/null
if strings "$package_root/bin/tmux-ide-host" | grep -E 'phase0-lanes|phase0-ssh|phase1-client' >/dev/null; then
  echo "release host contains development-only phase drivers" >&2
  exit 1
fi
