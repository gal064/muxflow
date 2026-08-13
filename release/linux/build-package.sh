#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"

# This script produces a Linux desktop package and links against WebKitGTK, so
# it can only run on Linux. The guard matters beyond a clearer message: the
# native-build branch below is chosen by comparing the requested architecture to
# `uname -m`, which on an Apple Silicon Mac reports arm64/aarch64. Requesting
# aarch64 there would take that branch, build a Darwin binary, and publish it
# inside a Linux release archive rather than failing.
if [[ $(uname -s) != Linux ]]; then
  echo "LINUX_HOST_REQUIRED: release/linux/build-package.sh builds a Linux desktop package and must run on Linux; found $(uname -s)" >&2
  exit 78
fi

arch=${1:-$(uname -m)}
case "$arch" in
  x86_64|amd64)
    arch=x86_64
    rust_target=x86_64-unknown-linux-gnu
    ;;
  aarch64|arm64)
    arch=aarch64
    rust_target=aarch64-unknown-linux-gnu
    ;;
  *)
    echo "unsupported Linux architecture: $arch" >&2
    exit 64
    ;;
esac

version=$(node -e 'process.stdout.write(require("./apps/desktop/src-tauri/tauri.conf.json").version)')
source_date_epoch=${SOURCE_DATE_EPOCH:-1704067200}
output_dir=${ADE_RELEASE_OUTPUT_DIR:-"$repo_root/tmp/release"}
mkdir -p "$repo_root/tmp" "$output_dir"
work_root=${ADE_WORK_ROOT:-"$repo_root/tmp/work"}
mkdir -p "$work_root/package-builds" "$work_root/system-tmp"
work_fstype=$(findmnt -n -o FSTYPE -T "$work_root")
case "$work_fstype" in
  tmpfs|ramfs) echo "release work root must be disk-backed: $work_root" >&2; exit 78 ;;
esac
export TMPDIR="$work_root/system-tmp"
export CARGO_INCREMENTAL=0
build_root=$(mktemp -d "$work_root/package-builds/phase8-package.XXXXXX")
trap 'rm -rf "$build_root"' EXIT
target_root=${CARGO_TARGET_DIR:-"$repo_root/target"}
[[ "$target_root" == /* ]] || target_root="$repo_root/$target_root"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$repo_root=/workspace/tmux-agent-ide --remap-path-prefix=$target_root=/workspace/target"

native_arch=$(uname -m)
case "$native_arch" in
  amd64) native_arch=x86_64 ;;
  arm64) native_arch=aarch64 ;;
esac

build_frontend_reproducibly() {
  rm -rf apps/desktop/dist
  (
    cd apps/desktop
    ./node_modules/.bin/tsc -b
    ./node_modules/.bin/vite build
  )
  # Tauri's embedded-asset context observes directory enumeration order. Vite
  # writes chunks concurrently, so identical content can otherwise produce a
  # different CSP-hash order in the final ELF. Re-extract a sorted archive to
  # give the embedder a stable directory order.
  local frontend_tar="$build_root/frontend.tar"
  tar --sort=name -C apps/desktop/dist -cf "$frontend_tar" .
  rm -rf apps/desktop/dist
  mkdir -p apps/desktop/dist
  tar -xf "$frontend_tar" -C apps/desktop/dist
}

if [[ "$arch" == "$native_arch" ]]; then
  build_frontend_reproducibly
  cargo build --locked --release -p tmux-agent-desktop
  binary_dir="$target_root/release"
else
  target_libdir=$(rustc --print target-libdir --target "$rust_target" 2>/dev/null || true)
  if [[ -z "$target_libdir" || ! -d "$target_libdir" ]]; then
    echo "ARM64_BLOCKER: Rust target $rust_target is not installed for the active toolchain" >&2
    exit 69
  fi
  linker=${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc}
  if ! command -v "$linker" >/dev/null 2>&1; then
    echo "ARM64_BLOCKER: cross linker $linker and an ARM64 WebKitGTK/GTK sysroot are required" >&2
    exit 69
  fi
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$linker"
  build_frontend_reproducibly
  cargo build --locked --release --target "$rust_target" -p tmux-agent-desktop
  binary_dir="$target_root/$rust_target/release"
fi

host_binary=${ADE_HOST_BINARY_OVERRIDE:-}
if [[ -z "$host_binary" ]]; then
  host_binary="$build_root/compatible-host/tmux-ide-host"
  release/linux/build-compatible-host.sh "$arch" "$host_binary" > "$build_root/compatible-host.log"
fi

package_name="tmux-agent-ide-$version-linux-$arch"
package_root="$build_root/$package_name"
mkdir -p "$package_root/bin" "$package_root/share/applications" "$package_root/share/icons/hicolor/256x256/apps"
install -m 0755 "$binary_dir/tmux-agent-desktop" "$package_root/bin/tmux-agent-desktop"
install -m 0755 "$host_binary" "$package_root/bin/tmux-ide-host"
install -m 0755 "$host_binary" "$package_root/bin/tmux-ide-host-$arch"
if [[ "$arch" == x86_64 && -n "${ADE_HOST_HELPER_AARCH64_OVERRIDE:-}" ]]; then
  install -m 0755 "$ADE_HOST_HELPER_AARCH64_OVERRIDE" "$package_root/bin/tmux-ide-host-aarch64"
elif [[ "$arch" == aarch64 && -n "${ADE_HOST_HELPER_X86_64_OVERRIDE:-}" ]]; then
  install -m 0755 "$ADE_HOST_HELPER_X86_64_OVERRIDE" "$package_root/bin/tmux-ide-host-x86_64"
fi
install -m 0755 release/linux/install.sh "$package_root/install.sh"
install -m 0755 release/linux/uninstall.sh "$package_root/uninstall.sh"
install -m 0644 release/linux/tmux-agent-ide.desktop.in "$package_root/share/applications/tmux-agent-ide.desktop.in"
install -m 0644 apps/desktop/src-tauri/icons/icon.png "$package_root/share/icons/hicolor/256x256/apps/tmux-agent-ide.png"
install -m 0644 LICENSE "$package_root/LICENSE" 2>/dev/null || true

(
  cd "$package_root"
  find . -type f ! -name SHA256SUMS -print0 \
    | LC_ALL=C sort -z \
    | xargs -0 sha256sum > SHA256SUMS
)

mkdir -p "$output_dir"
archive="$output_dir/$package_name.tar.gz"
tar --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 --numeric-owner \
  --pax-option=delete=atime,delete=ctime -C "$build_root" -cf - "$package_name" \
  | gzip -n > "$archive"
(cd "$output_dir" && sha256sum "$(basename "$archive")") > "$archive.sha256"
printf '%s\n' "$archive"
