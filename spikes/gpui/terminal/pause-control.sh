#!/usr/bin/env bash
# Control for QA defect: does a TUI corrupt after a resize during flood without pause-after?
# Runs nano in one of 8 panes, floods the rest, resizes twice, stops the flood, then
# captures a frame and tmux's own pane text, with and without pause-after.
OUT="${1:?usage: pause-control.sh OUT_DIR}"; mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cd "$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
export SPIKE_SCREEN=1600x1000x24
source spikes/gpui/env.sh >/dev/null 2>&1 || exit 1
# Never fall through to the caller's real display.
case "${XDG_RUNTIME_DIR:-}" in /tmp/muxflow-spike-env.*) ;; *) echo "no private display" >&2; exit 1 ;; esac
export TMUX_TMPDIR="$XDG_RUNTIME_DIR/tmux"; mkdir -p "$TMUX_TMPDIR"; unset TMUX
PID=""
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; tmux -L muxflow-qa-t04 kill-server 2>/dev/null; spike_env_down; }
trap cleanup EXIT
T() { tmux -L muxflow-qa-t04 "$@"; }
BIN="$PWD/spikes/gpui/target/release/spike-terminal"
pass() {
  local label="$1"; shift
  env "$@" "$BIN" run --panes 8 --socket muxflow-qa-t04 > $OUT/$label.log 2>&1 &
  PID=$!
  sleep 3
  local WID; WID=$(xdotool search --onlyvisible --name '.*' 2>/dev/null | tail -1)
  local W; W=$(T list-windows -t muxflow-spike -F '#{window_panes} #{window_id}' | sort -rn | head -1 | cut -d' ' -f2)
  mapfile -t PANES < <(T list-panes -F '#{pane_id}' -t "$W")
  printf 'line one\nline two\nline three\n' > $OUT/file.txt
  T send-keys -t "${PANES[1]}" "nano $OUT/file.txt" Enter
  sleep 2
  for i in 0 2 3 4 5 6 7; do T send-keys -t "${PANES[$i]}" "$BIN flood-writer" Enter; done
  sleep 8
  xdotool windowsize "$WID" 1000 620; sleep 3
  xdotool windowsize "$WID" 1280 800; sleep 3
  for i in 0 2 3 4 5 6 7; do T send-keys -t "${PANES[$i]}" C-c; done
  sleep 12
  spike_capture $OUT/$label.png
  T capture-pane -p -t "${PANES[1]}" > $OUT/$label-nano.txt
  T list-panes -t "$W" -F '#{pane_id} #{pane_width}x#{pane_height}' > $OUT/$label-panes.txt
  kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; PID=""
  T kill-server 2>/dev/null
  sleep 2
}
pass pause SPIKE_CONTROL=with-pause
pass nopause SPIKE_NO_PAUSE=1
echo "pause-control: frames and tmux truth in $OUT"
