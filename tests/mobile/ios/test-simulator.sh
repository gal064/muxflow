#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$repo_root"
evidence="$repo_root/tmp/ios-evidence"
mkdir -p "$evidence"
# A short scratch path is required by the host daemon's Unix-domain socket.
fixture_root=$(mktemp -d /tmp/mxi-XXXXXX)
export ADE_HOST_RUNTIME_DIR="$fixture_root/runtime"
export ADE_TMUX_SOCKET_NAME="ios-${fixture_root##*/}"
mkdir -p "$ADE_HOST_RUNTIME_DIR" "$fixture_root/work"
git -C "$fixture_root/work" init -q
printf '# iOS fixture document\n\nRendered over the bulk SSH channel.\n' > "$fixture_root/work/README.md"
tmux -L "$ADE_TMUX_SOCKET_NAME" -f /dev/null new-session -d -s primary -n shell -c "$fixture_root/work" 'exec bash --norc --noprofile'
uv run --with paramiko==4.0.0 --no-project tests/mobile/ios/ssh-fixture.py \
  --directory "$fixture_root" --host-binary "$repo_root/target/debug/muxflow-host" > "$evidence/fixture.log" 2>&1 &
fixture_pid=$!
cleanup() {
  if [ -n "${device_id:-}" ]; then
    xcrun simctl spawn "$device_id" log show --last 10m --style compact --info --debug \
      --predicate 'process == "Muxflow"' > "$evidence/app.log" || true
  fi
  kill "$fixture_pid" 2>/dev/null || true
  "$repo_root/target/debug/muxflow-host" daemon-stop || true
  tmux -L "$ADE_TMUX_SOCKET_NAME" kill-server || true
  rm -rf "$fixture_root"
}
trap cleanup EXIT
for attempt in $(seq 1 60); do
  if [ -f "$fixture_root/fixture.json" ]; then break; fi
  sleep 1
done
fixture_value() { node -e 'process.stdout.write(String(require(process.argv[1])[process.argv[2]]))' "$fixture_root/fixture.json" "$1"; }
# Maestro text selectors are regular expressions. Escape the base64 '+' so
# this asserts the exact key even when a disposable fingerprint contains it.
fingerprint=$(node -e 'const f=require(process.argv[1]);const hash=Buffer.from(f.fingerprint,"hex").toString("base64").replace(/=+$/, "");process.stdout.write("^SHA256:"+hash.replace(/\+/g,"\\+")+"$")' "$fixture_root/fixture.json")
device_id=$(cat "$evidence/device-id.txt")
maestro --device "$device_id" test --format junit --output "$evidence/none-auth.xml" \
  --test-output-dir "$evidence/none-auth" -e "SSH_PORT=$(fixture_value none)" -e "HOST_FINGERPRINT=$fingerprint" tests/mobile/ios/none-auth.yaml
# xterm paints on a canvas. Confirm submitted commands independently at tmux,
# alongside the terminal screenshot and real host file contents.
test "$(cat "$fixture_root/work/ios-e2e.marker")" = ran
test "$(cat "$fixture_root/work/ios-resumed.marker")" = resumed
tmux -L "$ADE_TMUX_SOCKET_NAME" capture-pane -p -t primary > "$evidence/none-terminal.txt"
rg 'ios-e2e-ok' "$evidence/none-terminal.txt"
touch "$fixture_root/drop-connections"
maestro --device "$device_id" test --format junit --output "$evidence/recovery.xml" \
  --test-output-dir "$evidence/recovery" tests/mobile/ios/recover.yaml
test "$(cat "$fixture_root/work/ios-recovered.marker")" = recovered
rm "$fixture_root/work/ios-e2e.marker" "$fixture_root/work/ios-resumed.marker"
maestro --device "$device_id" test --format junit --output "$evidence/key-generation.xml" \
  --test-output-dir "$evidence/key-generation" tests/mobile/ios/generate-key.yaml
# Copy exports only the public key through the product UI.
xcrun simctl pbpaste "$device_id" > "$fixture_root/authorized_keys"
rg '^ssh-ed25519 [A-Za-z0-9+/]+=* muxflow-mobile$' "$fixture_root/authorized_keys" > /dev/null
maestro --device "$device_id" test --format junit --output "$evidence/key-auth.xml" \
  --test-output-dir "$evidence/key-auth" -e "SSH_PORT=$(fixture_value publickey)" -e "HOST_FINGERPRINT=$fingerprint" tests/mobile/ios/key-auth.yaml
test "$(cat "$fixture_root/work/ios-e2e.marker")" = ran
test "$(cat "$fixture_root/work/ios-resumed.marker")" = resumed
tmux -L "$ADE_TMUX_SOCKET_NAME" capture-pane -p -t primary > "$evidence/key-terminal.txt"
rg 'ios-e2e-ok' "$evidence/key-terminal.txt"
xcrun simctl io "$device_id" screenshot "$evidence/final.png"
