#!/usr/bin/env bash
#
# Phase 13.5 QA: the recommended tmux window naming, against real tmux servers
# on isolated sockets.
#
# Three fixtures, because the interesting cases are not "does set-hook work":
#
#   stock   a `tmux -f /dev/null` server, which is also the Phase 13.4b
#           fresh-machine baseline — names must follow agent pane titles in the
#           app *and* in a plain client, since the name lives in tmux
#   twice    the same server, set up again — one hook, not two
#   user     a server whose own config already syncs pane titles — must be left
#           exactly as the user wrote it, exemptions and all
#
# Never touches the developer's own tmux servers: every server here is created
# and killed on a socket named for this run.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
helper=${ADE_HELPER:-"$here/../../target/debug/tmux-ide-host"}
socket="ade-phase13-naming-$$"
user_socket="ade-phase13-naming-user-$$"
work=$(mktemp -d "${TMPDIR:-/tmp}/ade-phase13-naming.XXXXXX")

cleanup() {
  tmux -L "$socket" kill-server 2>/dev/null || true
  tmux -L "$user_socket" kill-server 2>/dev/null || true
  tmux -L "ade-phase13-naming-fmt-$$" kill-server 2>/dev/null || true
  tmux -L "ade-phase13-naming-mixed-$$" kill-server 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

[[ -x "$helper" ]] || fail "helper not built at $helper (cargo build -p tmux-ide-host)"

# The daemon reaches tmux through the same socket name the app connects on.
export ADE_TMUX_SOCKET_NAME="$socket"

echo "tmux: $(tmux -V)"

# ── stock ───────────────────────────────────────────────────────────────────
# The pane runs a process genuinely named `claude`, because the recommendation
# is scoped to agent panes and tmux matches on the running command's own name —
# a shell script or a symlink reports its interpreter or its target instead.
# The helper itself is the most convenient real binary to borrow: copied under
# the agent's name, `hook ingest` blocks reading stdin and never contacts
# anything, so the pane holds a live process called `claude` and nothing else
# happens. A shell is used for the negative case below.
mkdir -p "$work/bin"
cp "$helper" "$work/bin/claude"
tmux -f /dev/null -L "$socket" new-session -d -s ade-phase13 -x 80 -y 24 \
  "$work/bin/claude hook ingest --adapter codex"
sleep 1
pane=$(tmux -L "$socket" list-panes -t ade-phase13 -F '#{pane_id}' | head -1)
before=$(tmux -L "$socket" display-message -p '#{window_name}')
echo "stock default window name: $before"

"$helper" host-naming > "$work/apply.json" || fail "host-naming failed"
grep -q '"outcome":"applied"' "$work/apply.json" || fail "expected applied, got $(cat "$work/apply.json")"

hooks=$(tmux -L "$socket" show-options -g pane-title-changed)
echo "hook after apply: $hooks"
[[ "$hooks" == *"rename-window"* ]] || fail "the recommended hook was not set"

# A pane title is how Claude Code and Codex announce what they are working on.
# Setting it must move the tmux window name — which is what a plain client
# sees, not only the app. `select-pane -T` is the same pane-title change an
# agent's OSC escape produces, without a shell prompt racing it.
tmux -L "$socket" select-pane -t "$pane" -T inductive
sleep 0.5
named=$(tmux -L "$socket" display-message -p -t "$pane" '#{window_name}')
echo "window name after the pane announced itself: $named"
[[ "$named" == "inductive" ]] || fail "window name did not follow the pane title (got '$named')"

# And what a plain tmux client would print for that window, since the contract
# is that the name is right in tmux rather than prettified in the app.
listed=$(tmux -L "$socket" list-windows -F '#{window_name}')
[[ "$listed" == *"inductive"* ]] || fail "a plain client does not see the name (got '$listed')"
echo "plain client sees: $listed"

# A non-agent pane must be left alone. An unscoped hook renamed *every* window
# to whatever its program put in the terminal title — a shell prompt's
# "user@host:~/path" on the field machine — and `rename-window` also turns
# tmux's own automatic renaming off for that window permanently, which outlives
# this app's connection. The guard is what makes this a recommendation rather
# than a change to how the user's shell windows are named.
shell_window=$(tmux -L "$socket" new-window -t ade-phase13 -P -F '#{window_id}')
shell_pane=$(tmux -L "$socket" list-panes -t "$shell_window" -F '#{pane_id}' | head -1)
tmux -L "$socket" select-pane -t "$shell_pane" -T "dev@host:~/somewhere"
sleep 0.5
shell_name=$(tmux -L "$socket" display-message -p -t "$shell_window" '#{window_name}')
echo "non-agent window kept its own name: $shell_name"
[[ "$shell_name" != "dev@host:~/somewhere" ]] || fail "a non-agent window took its pane title"

# ── twice ───────────────────────────────────────────────────────────────────
"$helper" host-naming > "$work/again.json" || fail "second host-naming failed"
grep -q '"outcome":"alreadyCurrent"' "$work/again.json" \
  || fail "a second run must recognise its own hook, got $(cat "$work/again.json")"
count=$(tmux -L "$socket" show-options -g pane-title-changed | grep -c 'rename-window')
[[ "$count" == "1" ]] || fail "applying twice left $count hooks"
echo "applied twice: $count hook"

# ── user config present ─────────────────────────────────────────────────────
cat > "$work/tmux.conf" <<'CONF'
set-hook -g pane-title-changed 'rename-window "#{?#{==:#{window_name},git},git,#{pane_title}}"'
CONF
ADE_TMUX_SOCKET_NAME="$user_socket" tmux -f "$work/tmux.conf" -L "$user_socket" new-session -d -s ade-phase13-user -x 80 -y 24
theirs=$(tmux -L "$user_socket" show-options -g pane-title-changed)
ADE_TMUX_SOCKET_NAME="$user_socket" "$helper" host-naming > "$work/user.json" || fail "host-naming failed against a configured server"
grep -q '"outcome":"userConfigured"' "$work/user.json" \
  || fail "expected userConfigured, got $(cat "$work/user.json")"
after=$(tmux -L "$user_socket" show-options -g pane-title-changed)
[[ "$theirs" == "$after" ]] || fail "the user's own hook was modified"
echo "user config left untouched: $after"

# Mixed: our hook plus one of the user's. `set-hook -g` replaces the whole
# array rather than appending, so treating this as "ours to update" destroys
# theirs. Any entry that is not ours makes the whole hook theirs.
mixed_socket="ade-phase13-naming-mixed-$$"
cat > "$work/mixed.conf" <<'CONF'
set-hook -g pane-title-changed 'if -F "#{==:#{pane_current_command},claude}" "rename-window \"#{pane_title}\""'
set-hook -ga pane-title-changed 'display-message -p "user hook"'
CONF
tmux -f "$work/mixed.conf" -L "$mixed_socket" new-session -d -s ade-phase13-mixed -x 80 -y 24
theirs_mixed=$(tmux -L "$mixed_socket" show-options -g pane-title-changed)
ADE_TMUX_SOCKET_NAME="$mixed_socket" "$helper" host-naming > "$work/mixed.json" \
  || fail "host-naming failed against a mixed server"
grep -q '"outcome":"userConfigured"' "$work/mixed.json" \
  || fail "expected userConfigured for a server carrying a user hook alongside ours, got $(cat "$work/mixed.json")"
[[ "$theirs_mixed" == "$(tmux -L "$mixed_socket" show-options -g pane-title-changed)" ]] \
  || fail "a user hook sharing the option with ours was replaced"
tmux -L "$mixed_socket" kill-server
echo "mixed hook left untouched"

# Removing: withdrawing consent has to take the tmux half back off too, and
# must not touch a hook that was never ours.
"$helper" host-naming --remove > "$work/removed.json" || fail "host-naming --remove failed"
grep -q '"outcome":"removed"' "$work/removed.json" \
  || fail "expected removed, got $(cat "$work/removed.json")"
[[ -z "$(tmux -L "$socket" show-options -g pane-title-changed | grep rename-window || true)" ]] \
  || fail "the hook survived its own removal"
echo "removed on withdrawal"

# The other mechanism that reaches the same result. A user who put pane titles
# into `automatic-rename-format` has configured this as deliberately as one who
# wrote the hook, and must not be overridden either.
format_socket="ade-phase13-naming-fmt-$$"
cat > "$work/format.conf" <<'CONF'
setw -g automatic-rename-format "#{pane_title}"
CONF
tmux -f "$work/format.conf" -L "$format_socket" new-session -d -s ade-phase13-fmt -x 80 -y 24
ADE_TMUX_SOCKET_NAME="$format_socket" "$helper" host-naming > "$work/format.json" \
  || fail "host-naming failed against a format-configured server"
grep -q '"outcome":"userConfigured"' "$work/format.json" \
  || fail "expected userConfigured for an automatic-rename-format that syncs titles, got $(cat "$work/format.json")"
[[ -z "$(tmux -L "$format_socket" show-options -g pane-title-changed | grep rename-window || true)" ]] \
  || fail "a hook was added over a server that already syncs titles by format"
tmux -L "$format_socket" kill-server
echo "automatic-rename-format detected as equivalent"

echo "PASS: recommended tmux window naming"
