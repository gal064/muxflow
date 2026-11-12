#!/usr/bin/env bash
# Scenario 1, sizes. Writes <results>/sizes.md for summarize.py.
#   sizes.sh <results-dir> [target-release-dir]
# Sizes: the binary as built (release profile, as release/linux/build-package.sh
# ships it), the same binary with `strip --strip-all`, and gzip -9 of the
# shipped binary (download size). The package row tars each app the way the
# Linux release tarball lays it out: the app, the helper, and the helper's
# per-arch copy. Tauri also needs the system WebKitGTK stack, listed separately.
set -euo pipefail
results="${1:?results dir}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target="${2:-$here/../../../target/release}"
work="$(mktemp -d "$results/.sizes.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mb() { awk -v b="$1" 'BEGIN { printf "%.1f", b / 1048576 }'; }
size() { stat -c %s "$1"; }

row() {
  local label=$1 binary=$2
  strip --strip-all -o "$work/stripped" "$binary"
  gzip -9 -c "$binary" > "$work/gz"
  printf '| %s | %s | %s | %s |\n' "$label" "$(mb "$(size "$binary")")" \
    "$(mb "$(size "$work/stripped")")" "$(mb "$(size "$work/gz")")"
}

package() {
  local app=$1 binary=$2
  mkdir -p "$work/$app/bin"
  cp "$binary" "$work/$app/bin/"
  cp "$target/muxflow-host" "$work/$app/bin/muxflow-host"
  cp "$target/muxflow-host" "$work/$app/bin/muxflow-host-linux-$(uname -m)"
  tar -C "$work" -czf "$work/$app.tar.gz" "$app"
  mb "$(size "$work/$app.tar.gz")"
}

libs() {
  ldd "$1" | awk '{print $1}' | sed 's/\.so.*//' | sort -u | tr '\n' ' '
}

pkgsize() {
  if command -v pacman >/dev/null; then
    pacman -Qi "$1" 2>/dev/null | awk -F': ' '/^Installed Size/ {print $2}'
  elif command -v dpkg-query >/dev/null; then
    dpkg-query -W -f='${Installed-Size} KiB' "$1" 2>/dev/null
  fi
}

{
  echo "## Sizes (Linux $(uname -m))"
  echo
  echo "| Binary | As built (MB) | strip --strip-all (MB) | gzip -9 of as-built (MB) |"
  echo "|---|---|---|---|"
  row "Tauri app (muxflow, frontend embedded)" "$target/muxflow"
  row "GPUI app (muxflow-gpui)" "$target/muxflow-gpui"
  row "Host helper (muxflow-host, shared)" "$target/muxflow-host"
  echo
  echo "Package as the Linux release tarball lays it out (app + helper + per-arch helper copy, tar.gz):"
  echo "Tauri $(package tauri "$target/muxflow") MB; GPUI $(package gpui "$target/muxflow-gpui") MB."
  echo "The release flow builds no GPUI package yet at this commit; its row uses the same layout."
  echo
  echo "System libraries the app links (ldd), beyond the helper:"
  echo
  echo "- Tauri: $(libs "$target/muxflow")"
  echo "- GPUI: $(libs "$target/muxflow-gpui")"
  echo
  echo "Installed size of the system web stack Tauri needs on this machine (context, not counted above):"
  for pkg in webkit2gtk-4.1 gtk3 libsoup3; do
    echo "- $pkg: $(pkgsize "$pkg" || echo n/a)"
  done
} > "$results/sizes.md"
echo "$results/sizes.md"
