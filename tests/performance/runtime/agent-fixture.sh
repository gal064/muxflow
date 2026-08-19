#!/usr/bin/env bash
# Agent-shaped pane fixture for the Stage 12.9 lanes: the echo lane (item 3,
# P12-U002) and the vt parity lane (item 4, P12-U003).
#
# It emits the shape claude/codex TUIs emit — the ALTERNATE screen, a
# cursor-addressed full-frame repaint wrapped in DEC 2026 synchronized output,
# with bold and dim attribute runs — once per second, and again in answer to
# every keystroke. One writer, so the keystroke answer can never be spliced into
# a timer repaint: the `KEY <n>` counter that ends the answer is what the driver
# waits for, and it exists in no other output.
#
# The frame is sized from the pane's own grid (tmux is authoritative for it), so
# the painted screen is directly comparable with `capture-pane`. To keep the
# wire volume the echo lane measures independent of pane geometry, the frame
# repaints the whole screen as many times as it takes to reach FRAME_MIN_BYTES.
# Repainting is idempotent: the screen after N repeats is the screen after one.
#
# Optional argument: a hold file. While it exists the fixture stops repainting —
# an idle agent TUI, which is the state a user is looking at when they see a
# corrupt pane. The parity lane needs that: a fixture that repaints through a
# recovery repairs a wrong delivery within a second and hides the defect.
set -u

hold_file="${1:-}"

# The ~4 KiB per frame the item 3 lane was specified against.
FRAME_MIN_BYTES=4096

size=$(stty size 2>/dev/null || true)
rows=${size%% *}
cols=${size##* }
[[ "$rows" =~ ^[0-9]+$ ]] && ((rows >= 4)) || rows=24
[[ "$cols" =~ ^[0-9]+$ ]] && ((cols >= 24)) || cols=80

# One column short of the grid: a run that ends exactly at the last column
# leaves the terminal in deferred-wrap state, which a capture cannot express and
# which would make the two sides of a parity comparison disagree about a
# terminal state neither of them got wrong.
width=$((cols - 1))
run=$((width / 3))
bold=$(printf 'B%.0s' $(seq 1 "$run"))
dim=$(printf 'd%.0s' $(seq 1 "$run"))
plain=$(printf 'x%.0s' $(seq 1 $((width - 2 * run))))
# Bold run, plain run, dim run: three attribute regions per row, so attribute
# parity has something to be wrong about.
line=$'\033[1m'"$bold"$'\033[22m'"$plain"$'\033[2m'"$dim"$'\033[22m'

body=""
for row in $(seq 1 "$rows"); do
  body+=$'\033['"$row"';1H'"$line"
done
repeats=1
while ((${#body} * repeats < FRAME_MIN_BYTES)); do
  repeats=$((repeats + 1))
done
frame=""
for ((repeat = 0; repeat < repeats; repeat++)); do
  frame+="$body"
done

# The alternate screen is what makes this an agent-TUI pane rather than a shell
# that prints a lot: it is the buffer tmux reports through `alternate_on` and
# captures through a different grid, and the seed path has to reproduce it.
printf '\033[?1049h\033[2J'

# Every repaint ends with a status line that changes: without it, every frame
# would be byte-identical and a screen that is arbitrarily stale would still
# compare equal to a current one. `FRAME` advances on the timer, `KEY` only ever
# advances in a keystroke answer, which is what the echo lane waits for.
painted=0
answered=0
while true; do
  if IFS= read -rsn1 -t 1 _key; then
    answered=$((answered + 1))
  else
    [[ -n "$hold_file" && -e "$hold_file" ]] && continue
    painted=$((painted + 1))
  fi
  printf '\033[?2026h%s\033[%d;1HFRAME %d  KEY %d  \033[?2026l' \
    "$frame" "$rows" "$painted" "$answered"
done
