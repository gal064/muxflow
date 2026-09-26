#!/usr/bin/env bash
# Drives the one-line installer (release/get.sh) through `curl | bash`
# against a local mirror of a GitHub release: fresh install, upgrade, and a
# checksum mismatch. Takes a Linux package built for this machine, e.g.
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
    | MUXFLOW_RELEASES_URL="http://127.0.0.1:$port" ADE_INSTALL_PREFIX="$prefix" bash
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

printf '%064d  %s\n' 0 "$name" > "$assets/SHA256SUMS"
if run_installer > "$work/bad.out" 2>&1; then
  echo "installer accepted a tarball with a wrong checksum" >&2
  exit 1
fi
grep -q "checksum mismatch" "$work/bad.out"
echo "checksum mismatch rejected: ok"
