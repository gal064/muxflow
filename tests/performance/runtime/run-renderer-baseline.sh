#!/usr/bin/env bash
# Black-box key-to-pixels baseline for the terminal renderer (Phase 0, T0.7).
# One instrument for both apps, on the same X display: renderer_baseline.py
# injects real keys through XTEST and polls GetImage until pixels change.
#
#   run-renderer-baseline.sh tauri|gpui [OUT_DIR]
#
# Display: by default sources spikes/gpui/env.sh (private Xvfb, lavapipe software
# Vulkan, WebKit on software GL (its DMABUF renderer stays enabled)). SPIKE_USE_CURRENT_DISPLAY=1 uses the X
# display in DISPLAY instead (for example Xwayland on a real GPU); nothing else
# changes. That display needs XTEST and a window the app can take input focus in.
#
# Isolation: private HOME and XDG roots, a private TMUX_TMPDIR and tmux socket
# (muxflow-qa-rb), and the host helper built from this checkout. Nothing here
# touches the owner's tmux, the installed muxflow-host daemon or any other window.
# Everything the run starts is found again by an environment marker and killed.
#
# Environment:
#   BASELINE_ONLY=<regex>   run only matching scenarios (see SCENARIOS in the .py)
#   BASELINE_QUICK=1        short run, to check that the harness works
#   RB_TAURI_BIN, RB_HELPER, RB_GPUI_BIN   override the binaries
#
# Build first (once):
#   TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo build --locked --release -p muxflow -p muxflow-host
#   cargo build --locked --release --manifest-path spikes/gpui/Cargo.toml -p spike-terminal
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
spikes="$repo/spikes/gpui"
app="${1:-}"
case "$app" in tauri | gpui) ;; *) echo "usage: $0 tauri|gpui [OUT_DIR]" >&2; exit 2 ;; esac
out="${2:-$here/renderer-baseline/results/$app-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

tauri_bin="${RB_TAURI_BIN:-$repo/target/release/muxflow}"
helper="${RB_HELPER:-$repo/target/release/muxflow-host}"
gpui_bin="${RB_GPUI_BIN:-$spikes/target/release/spike-terminal}"
flood_bin="${RB_GPUI_BIN:-$spikes/target/release/spike-terminal}"
for f in "$flood_bin" "$([ "$app" = tauri ] && echo "$tauri_bin" || echo "$gpui_bin")" "$([ "$app" = tauri ] && echo "$helper" || echo "$flood_bin")"; do
  [ -x "$f" ] || { echo "missing $f; build it first (see the header of this script)" >&2; exit 1; }
done
command -v xdotool >/dev/null || { echo "xdotool is required (window lookup)" >&2; exit 1; }
command -v tmux >/dev/null || { echo "tmux is required" >&2; exit 1; }

if [ "${SPIKE_USE_CURRENT_DISPLAY:-0}" = 1 ]; then
  : "${DISPLAY:?SPIKE_USE_CURRENT_DISPLAY=1 needs DISPLAY}"
  echo "Using the caller's X display: DISPLAY=$DISPLAY"
else
  cd "$repo"
  # shellcheck disable=SC1091
  source "$spikes/env.sh"   # starts the private display and stops it on exit
fi

bin="$gpui_bin"
[ "$app" = tauri ] && bin="$tauri_bin"
{
  echo "app: $app ($bin)"
  echo "tmux: $(tmux -V)"
  echo "kernel: $(uname -r)"
  echo "cpus: $(nproc)"
  echo "commit: $(git -C "$repo" rev-parse HEAD)"
  echo "date: $(date -u +%FT%TZ)"
  echo "display: ${DISPLAY:-}  current-display=${SPIKE_USE_CURRENT_DISPLAY:-0}"
  echo "vk: ${VK_DRIVER_FILES:-unset}  libgl-software: ${LIBGL_ALWAYS_SOFTWARE:-default(1 on the private display)}"
  echo "load at start: $(cut -d' ' -f1-3 /proc/loadavg)"
} >"$out/environment.txt"

quick=()
[ "${BASELINE_QUICK:-0}" = 1 ] && quick=(--quick)
status=0
uv run --no-project --python 3 python "$here/renderer_baseline.py" run \
  --app "$app" --bin "$bin" --helper "$helper" --flood-bin "$flood_bin" \
  --out "$out" --scenarios "${BASELINE_ONLY:-.}" "${quick[@]}" || status=$?
uv run --no-project --python 3 python "$here/renderer_baseline.py" summarize "$out" | tee "$out/summary.md"
echo "Results: $out"
exit "$status"
