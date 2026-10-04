#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
work="$repo_root/tmp/ios-native-test"
mkdir -p "$work"
export TMPDIR="$work"
if [ ! -d "$work/libssh2-1_11_1" ]; then
  git clone --depth 1 --branch libssh2-1.11.1 https://github.com/libssh2/libssh2.git "$work/libssh2-1_11_1"
fi
# Reset only the fixture checkout's patched files so repeated local runs test
# the current patch, not a cached version from an earlier implementation.
for source in include/libssh2.h src/libssh2_priv.h src/channel.c src/packet.c src/keepalive.c src/session.c src/kex.c; do
  git -C "$work/libssh2-1_11_1" show "HEAD:$source" > "$work/libssh2-1_11_1/$source"
done
(cd "$work" && ruby "$repo_root/apps/mobile/modules/muxflow-ssh/vendor/patch-libssh2.rb")
openssl_root="$(brew --prefix openssl@3)"
cmake -S "$work/libssh2-1_11_1" -B "$work/build" \
  -DCRYPTO_BACKEND=OpenSSL -DOPENSSL_ROOT_DIR="$openssl_root" \
  -DBUILD_SHARED_LIBS=OFF -DBUILD_EXAMPLES=OFF -DBUILD_TESTING=OFF
cmake --build "$work/build" --parallel 3
clang -fobjc-arc -fblocks -Wall -Wextra -Werror \
  -I "$repo_root/apps/mobile/modules/muxflow-ssh/ios" -I "$work/libssh2-1_11_1/include" \
  "$repo_root/tests/mobile/ios/native-transport.m" \
  "$repo_root/apps/mobile/modules/muxflow-ssh/ios/MFSSHTransport.m" \
  "$work/build/src/libssh2.a" -L "$openssl_root/lib" -lssl -lcrypto -lz \
  -framework Foundation -o "$work/native-transport"
rm -f "$work/fixture.json"
clang -Wall -Wextra -Werror -I "$work/libssh2-1_11_1/include" \
  "$repo_root/tests/mobile/ios/ssh-spike.c" "$work/build/src/libssh2.a" \
  -L "$openssl_root/lib" -lssl -lcrypto -lz -o "$work/ssh-spike"
uv run --with paramiko==4.0.0 --no-project "$repo_root/tests/mobile/ios/ssh-fixture.py" --directory "$work" > "$work/fixture.log" 2>&1 &
fixture_pid=$!
trap 'kill "$fixture_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 60); do
  if [ -f "$work/fixture.json" ]; then break; fi
  sleep 1
done
node - "$work" <<'NODE'
const {execFileSync} = require("node:child_process");
const work = process.argv[2];
const fixture = require(work + "/fixture.json");
for (const mode of ["none", "publickey"]) {
  execFileSync(work + "/ssh-spike", ["127.0.0.1", String(fixture[mode]), "fixture", fixture.fingerprint, mode === "none" ? "none" : fixture.key], {stdio: "inherit"});
}
NODE
"$work/native-transport" "$work/fixture.json"
