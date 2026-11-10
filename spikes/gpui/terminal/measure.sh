#!/usr/bin/env bash
# Repeatable measurement run for the terminal spike (T0.4). Every scenario runs
# the real path: private tmux server, `tmux -C` parsed by `crates/tmux-control`,
# one alacritty_terminal `Term` per pane, the GPUI element, in-process key
# dispatch. Writes one JSON per scenario, plus `summary.md`, into OUT_DIR.
#
#   measure.sh [OUT_DIR]        # defaults to terminal/results/<timestamp>
#
# Display:
#   default: sources spikes/gpui/env.sh (private Xvfb + lavapipe software Vulkan).
#   SPIKE_USE_CURRENT_DISPLAY=1 measure.sh ...  runs on whatever display the
#   caller's environment names (DISPLAY/WAYLAND_DISPLAY), for the root-level
#   real-GPU rerun. Nothing else differs, so the numbers are comparable.
#
# Safety: tmux only ever as `tmux -L muxflow-qa-t04` with a private TMUX_TMPDIR.
# It never touches the owner's tmux server or X display (unless asked above).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spikes="$(dirname "$here")"
repo="$(git -C "$here" rev-parse --show-toplevel)"
out="${1:-$here/results/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"  # the script changes directory below
bin="$spikes/target/release/spike-terminal"

cargo build --release --manifest-path "$spikes/Cargo.toml" -p spike-terminal

if [ "${SPIKE_USE_CURRENT_DISPLAY:-0}" = 1 ]; then
  echo "Using the caller's display: DISPLAY=${DISPLAY:-} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-}"
  trap 'tmux -L muxflow-qa-t04 kill-server 2>/dev/null || true' EXIT
else
  cd "$repo"
  # shellcheck disable=SC1091
  source "$spikes/env.sh"
  trap 'tmux -L muxflow-qa-t04 kill-server 2>/dev/null || true; spike_env_down' EXIT
fi
export TMUX_TMPDIR
TMUX_TMPDIR="$(mktemp -d /tmp/muxflow-t04.XXXXXX)"
trap "$(trap -p EXIT | sed -E "s/^trap -- '//; s/' EXIT\$//"); rm -rf '$TMUX_TMPDIR'" EXIT
unset TMUX

# The machine is shared and its load swings by an order of magnitude. Wait for it
# to come down (bounded) before each GUI scenario, and record what it was.
wait_quiet() {
  local waited=0
  while [ "$(cut -d. -f1 /proc/loadavg)" -ge "${SPIKE_QUIET_LOAD:-6}" ] && [ $waited -lt 240 ]; do
    sleep 5
    waited=$((waited + 5))
  done
  [ $waited -gt 0 ] && echo "   (waited ${waited}s for load)" | tee -a "$out/load.txt"
  return 0
}

failures=0
# SPIKE_ONLY=<regex> runs only the matching scenarios.
wanted() { [ -z "${SPIKE_ONLY:-}" ] || [[ "$1" =~ $SPIKE_ONLY ]]; }

run() {
  wanted "$1" || return 0
  wait_quiet
  local name="$1"
  shift
  rm -f "$out/$name.json"
  echo "== $name: $* (load: $(cut -d' ' -f1-3 /proc/loadavg))" | tee -a "$out/load.txt"
  timeout 400 "$bin" bench --out "$out/$name.json" --socket muxflow-qa-t04 "$@" \
    || { echo "FAILED: $name (exit $?)" | tee -a "$out/load.txt"; failures=$((failures + 1)); }
  sleep 2
}

run_headless() {
  wanted "$1" || return 0
  wait_quiet
  local name="$1"
  shift
  rm -f "$out/$name.json"
  echo "== $name: $* (load: $(cut -d' ' -f1-3 /proc/loadavg))" | tee -a "$out/load.txt"
  timeout 200 "$bin" control-baseline --out "$out/$name.json" --socket muxflow-qa-t04 "$@" \
    || { echo "FAILED: $name (exit $?)" | tee -a "$out/load.txt"; failures=$((failures + 1)); }
  sleep 2
}

run_headless baseline-tmux-only        --panes 8 --seconds 20
run_headless baseline-tmux-term        --panes 8 --seconds 20 --with-term
run_headless baseline-tmux-term-input  --panes 8 --seconds 20 --with-term --input-client
for r in 2 3; do
  run_headless "baseline-tmux-only-r$r"       --panes 8 --seconds 20
  run_headless "baseline-tmux-term-r$r"       --panes 8 --seconds 20 --with-term
  run_headless "baseline-tmux-term-input-r$r" --panes 8 --seconds 20 --with-term --input-client
done

run latency-1pane   --scenario latency --panes 1 --samples 600
run pixel-calib     --scenario pixel   --panes 1 --samples 200
run latency-8pane   --scenario latency --panes 8 --samples 600
run latency-8pane-filled --scenario latency --panes 8 --samples 600 --fill-panes
# Repeats: the machine is shared, so show the run-to-run spread.
run latency-1pane-r2 --scenario latency --panes 1 --samples 600
run latency-8pane-r2 --scenario latency --panes 8 --samples 600
run latency-1pane-r3 --scenario latency --panes 1 --samples 600
run latency-8pane-r3 --scenario latency --panes 8 --samples 600
run flood-7of8      --scenario flood   --panes 8 --seconds 60 --gap-min-ms 10 --gap-max-ms 40
# Echo in a flooded pane waits behind that pane's own tmux backlog, so it needs
# a longer per-key timeout than the 3 s default.
run flood-8of8      --scenario flood   --panes 8 --seconds 60 --flood-all --key-timeout-s 8
run flood-nopaint   --scenario flood   --panes 8 --seconds 60 --no-paint-panes
run resize-8pane    --scenario resize  --panes 8 --samples 30
run resize-8pane-filled --scenario resize --panes 8 --samples 30 --fill-panes
run switch-8pane    --scenario switch  --panes 8 --samples 60
# Under flood the layout and focus notifications queue behind pane output.
run resize-8pane-flood --scenario resize --panes 8 --samples 10 --flood-during --key-timeout-s 15
run switch-8pane-flood --scenario switch --panes 8 --samples 8 --flood-during --key-timeout-s 15
run idle-8pane      --scenario idle    --panes 8 --seconds 30
# RSS control: is growth real allocation, or khugepaged collapsing pages (THP)?
run flood-rss-thp   --scenario flood   --panes 8 --seconds 180 --gap-min-ms 10 --gap-max-ms 40
SPIKE_NO_THP=1 run flood-rss-nothp --scenario flood --panes 8 --seconds 180 --gap-min-ms 10 --gap-max-ms 40
run idle-rss-thp    --scenario idle    --panes 8 --seconds 120
SPIKE_NO_THP=1 run idle-rss-nothp --scenario idle --panes 8 --seconds 120
run onepane-cached  --scenario onepane --panes 8 --seconds 20
run onepane-nocache --scenario onepane --panes 8 --seconds 20 --no-view-cache
run ime             --scenario ime     --panes 1
run ligature        --scenario ligature --panes 1

{
  echo "tmux: $(tmux -V)"
  echo "kernel: $(uname -r)"
  echo "cpus: $(nproc)"
  echo "commit: $(git -C "$repo" rev-parse HEAD)"
  echo "date: $(date -u +%FT%TZ)"
  echo "thp: $(cat /sys/kernel/mm/transparent_hugepage/enabled) (scan_sleep_ms $(cat /sys/kernel/mm/transparent_hugepage/khugepaged/scan_sleep_millisecs), max_ptes_none $(cat /sys/kernel/mm/transparent_hugepage/khugepaged/max_ptes_none))"
} >"$out/environment.txt"
uv run --no-project python "$here/summarize.py" "$out" | tee "$out/summary.md"
echo "Results: $out"
if [ "$failures" -gt 0 ]; then
  echo "$failures scenario(s) FAILED; see $out/load.txt" >&2
  exit 1
fi
