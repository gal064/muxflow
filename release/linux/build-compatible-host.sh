#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
arch=${1:-$(uname -m)}
output=${2:-"$repo_root/tmp/compatible-host/tmux-ide-host"}
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported host-helper architecture: $arch" >&2; exit 64 ;;
esac
native=$(uname -m)
[[ "$native" != amd64 ]] || native=x86_64
[[ "$native" != arm64 ]] || native=aarch64
if [[ "$arch" != "$native" ]]; then
  echo "COMPATIBLE_HOST_BLOCKER: build the $arch helper on a native $arch runner" >&2
  exit 69
fi
command -v docker >/dev/null 2>&1 || {
  echo "COMPATIBLE_HOST_BLOCKER: Docker is required to build the Debian 12 baseline helper" >&2
  exit 69
}
case "$output" in
  "$repo_root"/*|"${ADE_WORK_ROOT:-$repo_root/tmp/work}"/*) ;;
  *) echo "compatible helper output must be inside the repository work root" >&2; exit 64 ;;
esac
work_root=${ADE_WORK_ROOT:-"$repo_root/tmp/work"}
mkdir -p "$work_root/cache/compatible-host" "$work_root/cache/cargo-home" "$(dirname "$output")"
source_digest=$(
  cd "$repo_root"
  # An isolated source copy can live beneath the developer checkout's ignored
  # tmp/work tree. Do not let Git walk upward and accidentally hash the parent
  # checkout; only use its index when this source root owns `.git` itself.
  if [[ -e "$repo_root/.git" ]] && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git ls-files --cached --others --exclude-standard -z -- \
      Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml apps/host crates
  else
    find Cargo.lock Cargo.toml rust-toolchain.toml rustfmt.toml apps/host crates \
      -type f -not -path '*/target/*' -not -path '*/tmp/*' -print0
  fi | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1
)
cache_dir="$work_root/cache/compatible-host/$arch/$source_digest"
target_dir="$cache_dir/target"
cached_binary="$cache_dir/tmux-ide-host"
mkdir -p "$cache_dir"
exec 9>"$cache_dir/build.lock"
flock 9
if [[ -x "$cached_binary" ]]; then
  install -m 0755 "$cached_binary" "$output"
  printf '%s\n' "$output"
  exit 0
fi
docker run --rm --user "$(id -u):$(id -g)" \
  -e CARGO_HOME=/artifact-cache/cargo-home \
  -e SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1704067200}" \
  -e RUSTFLAGS='--remap-path-prefix=/workspace=/workspace/tmux-agent-ide --remap-path-prefix=/artifact-build=/workspace/target' \
  -v "$repo_root:/workspace:ro" -v "$cache_dir:/artifact-build" \
  -v "$work_root/cache:/artifact-cache" \
  -w /workspace rust:1.97.1-slim-bookworm \
  cargo build --locked --release --bin tmux-ide-host \
    --target-dir /artifact-build/target
install -m 0755 "$target_dir/release/tmux-ide-host" "$cached_binary"
install -m 0755 "$cached_binary" "$output"

# Debian 12's glibc 2.36 is the declared dynamically-linked compatibility
# baseline. The runtime smoke below guards against accidentally packaging a
# helper rebuilt against the newer desktop distribution.
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$(dirname "$output"):/artifact:ro" debian:bookworm-slim \
  "/artifact/$(basename "$output")" version >/dev/null
printf '%s\n' "$output"
