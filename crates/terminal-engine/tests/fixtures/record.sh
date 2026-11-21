#!/usr/bin/env bash
# Records the control-mode fixtures replayed by tests/replay.rs.
#
# Launches tmux, so it must run inside spikes/gpui/qa-sandbox.sh:
#   qa-sandbox.sh run <name> bash crates/terminal-engine/tests/fixtures/record.sh <out-dir>
#
# The scenarios print only their own text, so the recordings hold no paths or
# machine names. Each one produces <name>.control and, per pane,
# <name>.pane-<id>.screen and <name>.pane-<id>.history.
set -euo pipefail

out="${1:?output directory}"
mkdir -p "$out"
export TMUX_TMPDIR
TMUX_TMPDIR="$(mktemp -d)"
work="$(mktemp -d)"

scenario_plain='seq 1 60; printf "last line, no newline"'
scenario_wrapped='for n in 1 2 3; do printf "%0200d\n" "$n"; done; printf "caf\303\251 \344\270\255\346\226\207 end\n"; printf "%079d\n" 5; printf "%080d\n" 6'
scenario_style='printf "\033[31mred\033[0m plain\n\033[1;44mbold on blue\033[K\033[0m\n"; printf "\033[10;5Hplaced\033[1;1Htop\033[2;3H\033[Kx\n"; printf "abc\rZ\n\ttabbed\n"'
scenario_alt='seq 1 30; printf "\033[?1049h\033[2J\033[Hin the alternate screen\n"; sleep 0.1; printf "\033[?1049lback on the main screen\n"'
scenario_alt_open='seq 1 30; printf "\033[?1049h\033[2J\033[5;5Hstill alternate"'
scenario_sync='printf "before\n\033[?2026h"; printf "held one\n"; sleep 0.05; printf "held two\n\033[?2026l"; printf "after\n"'
scenario_split='printf "left top\n"; tmux split-window -h "printf \"right side\n\"; seq 1 40; tmux wait-for -S right; sleep 300"; tmux wait-for right; seq 100 130'
scenario_resize='seq 1 40; tmux resize-window -x 60 -y 20; sleep 0.1; printf "\033[2J\033[H"; printf "%0100d\n" 7; seq 1 5'

record() {
  local name="$1" body="$2"
  local tmux=(tmux -f /dev/null -L fixture)
  printf '%s\n' "$body; tmux -L fixture wait-for -S finished; sleep 300" > "$work/$name.sh"
  # shellcheck disable=SC2016
  "${tmux[@]}" new-session -d -s s -x 80 -y 24 "tmux -L fixture wait-for start; sh $work/$name.sh"
  "${tmux[@]}" set -g window-size manual
  "${tmux[@]}" set -g status off
  "${tmux[@]}" set -g history-limit 10000
  mkfifo "$work/$name.stdin"
  "${tmux[@]}" -C attach -t s < "$work/$name.stdin" > "$out/$name.control" &
  local control=$!
  exec 3> "$work/$name.stdin"
  sleep 0.2
  "${tmux[@]}" wait-for -S start
  "${tmux[@]}" wait-for finished
  sleep 0.3
  while read -r pane; do
    local id="${pane#%}"
    "${tmux[@]}" capture-pane -p -t "$pane" > "$out/$name.pane-$id.screen"
    # With no history tmux clamps the range and answers with the top screen row.
    if [[ "$("${tmux[@]}" display-message -p -t "$pane" '#{history_size}')" == 0 ]]; then
      : > "$out/$name.pane-$id.history"
    else
      "${tmux[@]}" capture-pane -p -S - -E -1 -t "$pane" > "$out/$name.pane-$id.history"
    fi
  done < <("${tmux[@]}" list-panes -a -F '#{pane_id}')
  "${tmux[@]}" kill-server
  exec 3>&-
  wait "$control" 2>/dev/null || true
}

record plain "$scenario_plain"
record wrapped "$scenario_wrapped"
record style "$scenario_style"
record alt "$scenario_alt"
record alt_open "$scenario_alt_open"
record sync "$scenario_sync"
record split "$scenario_split"
record resize "$scenario_resize"
rm -rf "$work" "$TMUX_TMPDIR"
