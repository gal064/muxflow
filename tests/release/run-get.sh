#!/usr/bin/env bash
# Drives the one-line installer (release/get.sh) through `curl | bash`
# against a local mirror of a GitHub release: fresh install, upgrade, and a
# checksum mismatch, and pinned RCs. Takes a Linux package built for this machine, e.g.
#   tests/release/run-get.sh tmp/release/muxflow-0.1.1-linux-x86_64.tar.gz
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
tarball=$(realpath -e "${1:?usage: run-get.sh <muxflow-X.Y.Z-linux-ARCH.tar.gz>}")
name=$(basename "$tarball")
version=${name#muxflow-}
version=${version%%-linux-*}

mkdir -p "$repo_root/tmp"
work=$(mktemp -d "$repo_root/tmp/run-get.XXXXXX")
server_pid=
cleanup() {
  [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

mirror="$work/mirror"
assets="$mirror/download/v$version"
mkdir -p "$assets" "$mirror/latest/download"
cp "$tarball" "$assets/"
(cd "$assets" && sha256sum -- "$name" > SHA256SUMS)
printf '{"version": "%s", "url": "unused"}\n' "$version" > "$mirror/latest/download/latest.json"
cp "$repo_root/release/get.sh" "$mirror/install.sh"

port=$(uv run --no-project python -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
uv run --no-project python -m http.server "$port" --bind 127.0.0.1 --directory "$mirror" > "$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 50); do
  curl -fs -o /dev/null "http://127.0.0.1:$port/install.sh" && break
  sleep 0.1
done

prefix="$work/prefix"
run_installer() {
  curl -fsSL "http://127.0.0.1:$port/install.sh" \
    | MUXFLOW_RELEASES_URL="http://127.0.0.1:$port" ADE_INSTALL_PREFIX="$prefix" MUXFLOW_VERSION="${1:-}" bash
}

run_installer > "$work/install.out"
grep -q "Installed Muxflow under $prefix" "$work/install.out"
grep -q "$prefix/bin is not on your PATH" "$work/install.out"
[[ -x "$prefix/bin/muxflow" && -x "$prefix/bin/muxflow-host" ]]
grep -q "^Exec=\"$prefix/bin/muxflow\"$" "$prefix/share/applications/muxflow.desktop"
echo "fresh install: ok"

run_installer > "$work/upgrade.out"
grep -q "Installed Muxflow under $prefix" "$work/upgrade.out"
echo "reinstall/upgrade: ok"

run_installer "v$version" > "$work/pinned.out"
grep -q "Downloading Muxflow $version for" "$work/pinned.out"
echo "pinned stable install: ok"

rc="$version-rc.2"
rc_assets="$mirror/download/v$rc"
mkdir -p "$rc_assets"
cp "$assets/"* "$rc_assets/"
for pin in "$rc" "v$rc"; do
  run_installer "$pin" > "$work/rc.out"
  grep -q "Downloading Muxflow $rc for" "$work/rc.out"
  [[ -x "$prefix/bin/muxflow" && -x "$prefix/bin/muxflow-host" ]]
done
echo "pinned RC install, with and without v: ok"

for release_tag in "$version-ios" "$version-ios-internal" "$rc-ios" "$rc-ios-internal"; do
  mkdir -p "$mirror/download/v$release_tag"
  cp "$assets/"* "$mirror/download/v$release_tag/"
  for pin in "$release_tag" "v$release_tag"; do
    run_installer "$pin" > "$work/ios.out"
    grep -q "Downloading Muxflow $release_tag for" "$work/ios.out"
    [[ -x "$prefix/bin/muxflow" && -x "$prefix/bin/muxflow-host" ]]
  done
done
echo "pinned iOS release tags: ok"

# The public stable tag can carry -ios; latest installs must use the exact tag,
# while the package filename and application version remain the base semver.
for release_tag in "$version-ios" "$version-ios-internal"; do
  printf '{"version": "%s", "tag": "v%s", "url": "unused"}\n' "$version" "$release_tag" > "$mirror/latest/download/latest.json"
  run_installer > "$work/latest-ios.out"
  grep -q "Downloading Muxflow $release_tag for" "$work/latest-ios.out"
done
printf '{"version": "%s", "url": "unused"}\n' "$version" > "$mirror/latest/download/latest.json"
echo "latest stable iOS-tag install: ok"

for pin in "$version-beta.1" "$version-rc." "$version-ios-external" "$version-ios-rc.1" '../escape'; do
  if run_installer "$pin" > "$work/invalid.out" 2>&1; then
    echo "installer accepted invalid version: $pin" >&2
    exit 1
  fi
  grep -q 'invalid version' "$work/invalid.out"
done
echo "invalid versions rejected: ok"

printf '%064d  %s\n' 0 "$name" > "$assets/SHA256SUMS"
if run_installer > "$work/bad.out" 2>&1; then
  echo "installer accepted a tarball with a wrong checksum" >&2
  exit 1
fi
grep -q "checksum mismatch" "$work/bad.out"
echo "checksum mismatch rejected: ok"

printf '%064d  %s\n' 0 "$name" > "$rc_assets/SHA256SUMS"
if run_installer "$rc" > "$work/rc-bad.out" 2>&1; then
  echo "installer accepted an RC tarball with a wrong checksum" >&2
  exit 1
fi
grep -q "checksum mismatch" "$work/rc-bad.out"
echo "RC checksum mismatch rejected: ok"
