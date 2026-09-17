#!/bin/sh
set -eu

log=/tmp/ade-phase12-tmux-process.log
parent=$(sed -n '1p' "/proc/$PPID/comm" 2>/dev/null || printf unknown)
line=$parent
for argument in "$@"; do
  line="$line	<$argument>"
done
printf '%b\n' "$line" >> "$log"
exec /usr/bin/tmux "$@"
