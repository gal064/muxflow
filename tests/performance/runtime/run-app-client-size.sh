#!/usr/bin/env bash
# Stage 12.10 item 1, desktop half: does the *app* ask tmux for its own
# surface, and only that?
#
# Every other lane in this directory drives the host with numbers a script
# chose. This one launches the packaged desktop against an isolated tmux
# fixture and reads what tmux ends up with, which is the only way to exercise
# the computation that damaged the user's windows (P12-U006): the surface
# measurement, the cell metrics, the debounce and the dedupe all live in the
# renderer.
#
# The churn is applied from outside the app — split, zoom, unzoom, window
# switch — because that is the shape of the layout change the old formula
# multiplied. The gate is that the window size tmux reports never moves.
#
# The baseline is read from the running app rather than written here, so there
# is no expected size to keep in step with the renderer. For the record: at the
# app's default 1280x800 window this lane observed 135x30 before the row pitch
# was corrected, 135x39 with the 18.5px rows that correction first produced, and
# **135x40** now that the row is 18.0px (`evidence/phase12-11/row-pitch.md`,
# `evidence/phase12-12/glyph-jitter.md`). The last move is deliberate: an 18.5px
# row is 37 device pixels on a 2x display, and an odd device row is what
# stretched the whole terminal canvas by a pixel. A future change to the
# terminal's metrics moves this number again and this lane will still pass,
# which is correct: what it gates is that the number does not move *while the
# app is running*.
#
#   bash tests/performance/runtime/run-app-client-size.sh
set -euo pipefail
trap 'echo "app-client-size lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
rounds="${1:-3}"
idle_seconds="${2:-20}"

run_root="$(bash "$repo_root/tests/performance/runtime/setup-qa.sh" | tail -1)"
socket="$(grep -o "ADE_TMUX_SOCKET_NAME='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)"
tmux_tmpdir="$(grep -o "TMUX_TMPDIR='[^']*'" "$run_root/launch.sh" | cut -d"'" -f2)"
cleanup() { bash "$run_root/cleanup.sh" >/dev/null 2>&1 || true; }
trap cleanup EXIT

T() { env -u TMUX TMUX_TMPDIR="$tmux_tmpdir" tmux -L "$socket" "$@"; }
sizes() { T list-windows -a -F '#{window_width}x#{window_height}' | sort -u | tr '\n' ' '; }

# The fixture's session starts at tmux's own default. It is created *here*,
# before the app is launched, so the default is read from a server no app has
# touched: sampling it after the launch would race the app for its own baseline,
# and a lane that took the app's size as the default could never see it change.
# `launch.sh` creates the session only if it is missing, so this is its own
# fixture, not a second one.
env -u TMUX TMUX_TMPDIR="$tmux_tmpdir" tmux -L "$socket" -f /dev/null \
  new-session -d -s phase12 'exec bash' >/dev/null 2>&1 || true
env -u TMUX TMUX_TMPDIR="$tmux_tmpdir" tmux -L "$socket" -f /dev/null \
  new-window -d -t phase12 -n second 'exec bash' >/dev/null 2>&1 || true
default_size="$(sizes)"
[[ -n "${default_size// }" ]] || { echo "the fixture session never started" >&2; exit 1; }

nohup "$run_root/launch.sh" >/dev/null 2>&1 &
ready=""
for _ in $(seq 1 60); do
  if [[ -n "$(T list-clients -F '#{client_name}' 2>/dev/null)" && "$(sizes)" != "$default_size" ]]; then
    ready=yes
    break
  fi
  sleep 1
done
[[ -n "$ready" ]] || {
  echo "FAIL  app-client-size  the app never sized the tmux client (still $default_size after 60s)" >&2
  echo "artifacts: $run_root" >&2
  exit 1
}

samples=()
baseline="$(sizes)"
samples+=("start:$baseline")

for ((round = 1; round <= rounds; round++)); do
  # Alternating axes: a horizontal split is the one that exercises the
  # scrollbar allowance, which is charged to the surface once and spent by
  # every pane.
  if (( round % 2 == 1 )); then
    T split-window -v -t phase12:0 >/dev/null 2>&1
  else
    T split-window -h -t phase12:0 >/dev/null 2>&1
  fi
  sleep 2
  samples+=("split$round:$(sizes)")
  T resize-pane -Z -t phase12:0 >/dev/null 2>&1; sleep 2
  samples+=("zoom$round:$(sizes)")
  T resize-pane -Z -t phase12:0 >/dev/null 2>&1; sleep 2
  samples+=("unzoom$round:$(sizes)")
  T select-window -t "phase12:$((round % 2))" >/dev/null 2>&1; sleep 2
  samples+=("select$round:$(sizes)")
done
sleep "$idle_seconds"
samples+=("idle:$(sizes)")

panes="$(T list-panes -a -F '#{pane_id} #{pane_width}x#{pane_height}' | tr '\n' ' ')"
clients="$(T list-clients -F '#{client_name} #{client_width}' | tr '\n' ' ')"
printf '{\n  "label": "app-client-size-local",\n  "rounds": %s,\n  "idleSeconds": %s,\n  "tmuxDefaultBeforeApp": "%s",\n  "baseline": "%s",\n  "samples": [%s],\n  "panesAfter": "%s",\n  "clients": "%s"\n}\n' \
  "$rounds" "$idle_seconds" "${default_size% }" "${baseline% }" \
  "$(printf '"%s",' "${samples[@]}" | sed 's/,$//')" \
  "${panes% }" "${clients% }" >"$run_root/app-client-size.json"
cat "$run_root/app-client-size.json"

for sample in "${samples[@]}"; do
  [[ "${sample#*:}" == "$baseline" ]] || {
    echo "FAIL  app-client-size  the tmux window size moved: $sample (started $baseline)" >&2
    echo "artifacts: $run_root" >&2
    exit 1
  }
done
printf 'PASS  app-client-size  %s stayed %sthrough %s rounds of split/zoom/unzoom/window-switch and %ss idle\n' \
  "the tmux window" "$baseline" "$rounds" "$idle_seconds"
printf 'artifacts: %s\n' "$run_root"
