#!/usr/bin/env bash
set -euo pipefail

repo=${1:?usage: create-scale-fixture.sh REPO [TMUX_SOCKET]}
socket=${2:-}
if [[ -n "$socket" ]]; then
  # A named release fixture must never inherit and mutate the caller's tmux.
  unset TMUX
fi
mkdir -p "$repo/template"
git -C "$repo" init -q
git -C "$repo" config user.name 'Phase Eight'
git -C "$repo" config user.email phase8@example.test
printf '/bulk-*\n/template\n' > "$repo/.gitignore"
printf 'baseline\n' > "$repo/tracked"
git -C "$repo" add .gitignore tracked
git -C "$repo" commit -qm baseline

for file_index in $(seq -w 0 999); do
  : > "$repo/template/file-$file_index"
done

for directory_index in $(seq -w 0 249); do
  cp -al "$repo/template" "$repo/bulk-$directory_index"
done

tmux_cmd=(tmux)
if [[ -n "$socket" ]]; then
  tmux_cmd+=( -L "$socket" )
fi
"${tmux_cmd[@]}" kill-server >/dev/null 2>&1 || true
for session_index in $(seq -w 0 19); do
  session="phase8-$session_index"
  "${tmux_cmd[@]}" new-session -d -s "$session" -c "$repo" 'exec bash'
  for window_index in $(seq 1 4); do
    "${tmux_cmd[@]}" new-window -d -t "$session" -n "window-$window_index" -c "$repo" 'exec bash'
  done
done

# Keep exactly 50 panes producing at roughly 60 Hz while leaving their shells
# interactive. This exercises bounded terminal ingestion rather than merely
# counting idle panes.
live_panes=0
while IFS= read -r pane; do
  ((live_panes += 1))
  ((live_panes <= 50)) || break
  "${tmux_cmd[@]}" send-keys -t "$pane" \
    "while :; do printf 'phase8-live-%s\\r\\n' '$pane'; sleep 0.016; done &" Enter
done < <("${tmux_cmd[@]}" list-panes -a -F '#{pane_id}')
[[ "$live_panes" -ge 50 ]]

actual_files=$(find "$repo" -path "$repo/.git" -prune -o -type f -print | wc -l)
if (( actual_files < 250002 )); then
  echo "scale fixture has only $actual_files files" >&2
  exit 1
fi
printf 'files=%s sessions=20 windows=100 livePanes=50\n' "$actual_files"
