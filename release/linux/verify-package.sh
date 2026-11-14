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
for binary in "$package_root/bin/muxflow" "$package_root/bin/muxflow-host"; do
  readelf -h "$binary" | grep -F "Machine:                           $expected_machine" >/dev/null
done
for helper in "$package_root"/bin/muxflow-host-linux-x86_64 "$package_root"/bin/muxflow-host-linux-aarch64; do
  [[ -f "$helper" ]] || continue
  case "$helper" in
    *-x86_64) helper_machine='Advanced Micro Devices X86-64' ;;
    *-aarch64) helper_machine='AArch64' ;;
  esac
  readelf -h "$helper" | grep -F "Machine:                           $helper_machine" >/dev/null
done
# A thin 64-bit arm64 Mach-O: magic feedfacf, CPU type 0100000c, little-endian.
mac_helper="$package_root/bin/muxflow-host-macos-aarch64"
if [[ -f "$mac_helper" ]]; then
  [[ "$(od -An -tx1 -N8 "$mac_helper" | tr -d ' \n')" == cffaedfe0c000001 ]] || {
    echo "macOS helper is not an arm64 Mach-O executable" >&2
    exit 1
  }
fi
# The licence notices the shipped crates require (release/third-party-licenses.mjs):
# present, with the Apache-2.0 text and the GPUI and terminal crates it covers.
notices="$package_root/THIRD-PARTY-LICENSES"
[[ -s "$notices" ]] \
  && grep -Fq 'TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION' "$notices" \
  && grep -Eq '^gpui-pre [0-9]' "$notices" \
  && grep -Eq '^alacritty_terminal [0-9]' "$notices" || {
  echo "THIRD-PARTY-LICENSES is missing or incomplete" >&2
  exit 1
}
# The desktop is the GPUI app: its panic locations name the app's sources, and
# it links no WebKitGTK or GTK. This catches a package assembled from the old
# Tauri binary.
grep -aF 'apps/desktop/src/' "$package_root/bin/muxflow" >/dev/null || {
  echo "desktop is not the GPUI app" >&2
  exit 1
}
if readelf -d "$package_root/bin/muxflow" | grep -E 'NEEDED.*(webkit|gtk)' >/dev/null; then
  echo "desktop links WebKitGTK or GTK" >&2
  exit 1
fi
# The desktop is built on ubuntu-24.04 (glibc 2.39). A build on a newer
# distribution would install but fail to start on that baseline.
newest_glibc=$(readelf -V "$package_root/bin/muxflow" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -V | tail -n 1)
[[ "$(printf '%s\n2.39\n' "$newest_glibc" | sort -V | tail -n 1)" == 2.39 ]] || {
  echo "desktop needs glibc $newest_glibc; the baseline is 2.39" >&2
  exit 1
}
if strings "$package_root/bin/muxflow-host" | grep -E 'phase0-lanes|phase0-ssh|phase1-client' >/dev/null; then
  echo "release host contains development-only phase drivers" >&2
  exit 1
fi
