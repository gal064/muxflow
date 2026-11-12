#!/usr/bin/env bash
# Tauri vs GPUI desktop benchmark on macOS. Same scenarios as run-linux.sh.
#
#   run-macos.sh [bench_macos.py options]
#
# Build both apps from one commit first (README.md). Environment:
#   BENCH_ROOT        short scratch root (default /tmp/mb; macOS sockets cap at 103 bytes)
#   BENCH_SSH_CONFIG  ssh_config with a Host for the SSH target (enables --hosts ssh)
#   BENCH_SSH_TARGET  Host alias (default muxflow-bench)
#   BENCH_RECORDINGS  dir with claude/codex .data + .timing; when unset and the SSH
#                     host is configured, fetched from ~/bench/agents on that host
#   BENCH_METER       the built ScreenCaptureKit meter (tests/performance/runtime/macos/
#                     build-meter.sh), needed for typing latency
# Quit Safari (and other WebKit apps you can) first: WebKit XPC services are
# attributed to Tauri by a pid diff.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
root="${BENCH_ROOT:-/tmp/mb}"
mkdir -p "$root"
results="$root/results-macos"

for tool in tmux perl uv osascript screencapture footprint rustc; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 69; }
done
app="$repo/target/release/bundle/macos/Muxflow.app"
for path in "$app/Contents/MacOS/muxflow" "$repo/target/release/muxflow-gpui" "$repo/target/release/muxflow-host"; do
  [[ -x "$path" ]] || { echo "missing $path; build both apps first (README.md)" >&2; exit 69; }
done

args=(--root "$root" --repo "$repo" --results "$results")
if [[ -n "${BENCH_SSH_CONFIG:-}" ]]; then
  args+=(--ssh-config "$BENCH_SSH_CONFIG" --ssh-target "${BENCH_SSH_TARGET:-muxflow-bench}")
fi
recordings="${BENCH_RECORDINGS:-$root/recordings}"
if [[ ! -s "$recordings/claude.data" && -n "${BENCH_SSH_CONFIG:-}" ]]; then
  mkdir -p "$recordings"
  scp -q -F "$BENCH_SSH_CONFIG" "${BENCH_SSH_TARGET:-muxflow-bench}:bench/agents/{claude,codex}.{data,timing}" "$recordings/"
fi
if [[ -s "$recordings/claude.data" ]]; then
  mkdir -p "$recordings/bin"
  rustc --edition 2024 -O "$here/workloads/agent-replay.rs" -o "$recordings/bin/agent-replay"
  args+=(--recordings "$recordings")
fi
[[ -n "${BENCH_METER:-}" ]] && args+=(--meter "$BENCH_METER")

uv run --script "$here/bench_macos.py" "${args[@]}" "$@"

# Scenario 1, sizes.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mb() { awk -v b="$1" 'BEGIN { printf "%.1f", b / 1048576 }'; }
row() {
  strip -S -x -o "$work/s" "$2" 2>/dev/null || cp "$2" "$work/s"
  gzip -9 -c "$2" > "$work/gz"
  printf '| %s | %s | %s | %s |\n' "$1" "$(mb "$(stat -f %z "$2")")" "$(mb "$(stat -f %z "$work/s")")" "$(mb "$(stat -f %z "$work/gz")")"
}
{
  echo "## Sizes (macOS $(uname -m))"
  echo
  echo "| Binary | As built (MB) | strip -S -x (MB) | gzip -9 of as-built (MB) |"
  echo "|---|---|---|---|"
  row "Tauri app (muxflow, frontend embedded)" "$app/Contents/MacOS/muxflow"
  row "GPUI app (muxflow-gpui)" "$repo/target/release/muxflow-gpui"
  row "Host helper (muxflow-host)" "$repo/target/release/muxflow-host"
  echo
  echo "Muxflow.app bundle on disk: $(du -sk "$app" | awk '{printf "%.1f", $1/1024}') MB (includes the helper sidecar and Linux helpers)."
  echo "WKWebView is part of macOS, so Tauri ships no web engine here."
} > "$results/sizes.md"
uv run --script "$here/summarize.py" "$results"
