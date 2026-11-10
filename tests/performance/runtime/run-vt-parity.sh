#!/usr/bin/env bash
# Stage 12.9 item 4 lane (P12-U003): vt parity between the screen the host
# delivered and the screen tmux itself has.
#
# The fixture pairs a flood-able plain shell pane with an agent-TUI pane
# (alternate screen, cursor addressing, DEC 2026 brackets, bold/dim runs, 1 Hz
# repaint). At five quiesce points — seed, hide/reveal, pause/continue, an
# explicit seed request, and a full bridge reconnect — the driver replays every
# byte the host delivered for each pane through a vt parser sized to the pane's
# tmux grid and diffs the resulting screen against `capture-pane -p -e` rendered
# through a second parser of the same size. Text and attribute parity are both gates.
#
# S9 (docs/gpui-migration.md): the same delivered bytes also go through
# `alacritty_terminal`, the planned terminal engine, and are compared against
# `capture-pane` read without any parser. A third pane, in its own window so the
# geometry of the first two is untouched, paints wide characters, emoji, ZWJ and
# VS16 sequences, combining marks and box drawing, and is compared both as seeded
# and after a live repaint. A history-page check proves the engine marks composed
# scrollback as wrapped. Each is its own gate, reported separately from vt100.
#
# Isolated tmux socket + isolated runtime dir under /tmp (macOS caps UDS paths
# at 103 bytes); the user's tmux is never touched.
set -euo pipefail
trap 'echo "vt-parity lane failed at line $LINENO" >&2' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run_id="$(date -u +%H%M%S)-$$"
runtime="/tmp/adevtp-$run_id"
socket="ade-vt-parity-$$"
main_target="${CARGO_TARGET_DIR:-$repo_root/target}"
driver_target="${CARGO_TARGET_DIR:-$repo_root/tests/performance/runtime/perf-driver/target}"
host_binary="$main_target/release/muxflow-host"
driver_binary="$driver_target/release/performance-test-driver"
stall_seconds="${1:-8}"
# Under the 1 Hz repaint, "no new bytes for this long" is the pane standing
# still between frames.
quiesce_ms="${2:-700}"

daemon_pid=""
socket_path=""
cleanup() {
  [[ -n "$daemon_pid" ]] && kill -CONT "$daemon_pid" 2>/dev/null || true
  ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
    "$host_binary" daemon-stop >/dev/null 2>&1 || true
  [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null || true
  tmux -L "$socket" kill-server >/dev/null 2>&1 || true
  # tmux leaves the socket file behind after kill-server.
  [[ -n "$socket_path" ]] && rm -f "$socket_path" || true
}
trap cleanup EXIT

mkdir -p "$runtime"
chmod 0700 "$runtime"

cd "$repo_root"
cargo build --release --bin muxflow-host >"$runtime/cargo-host-build.log" 2>&1
cargo build --release --manifest-path tests/performance/runtime/perf-driver/Cargo.toml \
  >"$runtime/cargo-driver-build.log" 2>&1

# An explicit grid: the parity comparison is only as meaningful as the geometry
# both sides agree on, and a detached session's default size is not stated
# anywhere the lane can read it back from.
tmux -L "$socket" new-session -d -x 120 -y 36 -s vtparity 'exec bash --norc'
socket_path="$(tmux -L "$socket" display-message -p '#{socket_path}')"
# The hold file is how the driver stops the fixture repainting while it checks a
# recovery; the path is the driver's own convention ($runtime/agent-hold).
tmux -L "$socket" split-window -t vtparity \
  "exec bash $repo_root/tests/performance/runtime/agent-fixture.sh $runtime/agent-hold"

# The width fixture's rows need a taller grid than a third split would leave,
# and a window of its own keeps the two panes above exactly as they were.
tmux -L "$socket" new-window -d -t vtparity -n width \
  "stty -echo -icanon; exec $driver_binary vt-width-fixture"

ADE_PHASE1_TESTING=1 ADE_HOST_RUNTIME_DIR="$runtime" ADE_TMUX_SOCKET_NAME="$socket" \
  "$host_binary" daemon >"$runtime/daemon.log" 2>&1 &
daemon_pid=$!
sleep 1
kill -0 "$daemon_pid"

ADE_PHASE1_TESTING=1 "$driver_binary" vt-parity \
  --transport local \
  --host-binary "$host_binary" \
  --runtime "$runtime" \
  --tmux-socket "$socket" \
  --primary vtparity \
  --label "vt-parity-local-${stall_seconds}s" \
  --stall-seconds "$stall_seconds" \
  --quiesce-ms "$quiesce_ms" \
  --stall-script "kill -STOP $daemon_pid" \
  --restore-script "kill -CONT $daemon_pid" \
  >"$runtime/vt-parity.json"

cat "$runtime/vt-parity.json"

jq -r '
  "attribute mismatches: " + (
    [.quiescePoints | to_entries[] | "\(.key)=\(.value.attributeMismatchCells)"] | join(" ")
  )
' "$runtime/vt-parity.json"

# Attributes are gated, not just reported: mid-word bold/dim tearing is one of
# the three artifacts item 4 exists to close, and a lane that prints an
# attribute regression without failing on it would let that one back in.
jq -r '
  "pause recovery: without other output=\(.pauseRecoveredWithoutOtherOutput)" +
  " after width repaint=\(.pauseRecoverySeeded)" +
  " panes paused=\(.panesPausedAfterRestore)"
' "$runtime/vt-parity.json"

status=0
if jq -e '
  (.quiescePoints | has("seed") and has("hideReveal") and has("pauseContinue")
    and has("recovery") and has("reconnect"))
  and (.quiescePoints | to_entries | all(.value.textMismatchRows == 0))
  and (.quiescePoints | to_entries | all(.value.attributeMismatchCells == 0))
  and (.connectionWideResyncEvents == 0)
  and (.sequenceGapObserved | not)
  and .pauseRecoverySeeded
' "$runtime/vt-parity.json" >/dev/null; then
  printf 'PASS  vt-parity  delivered screens matched tmux at all five quiesce points\n'
else
  echo "FAIL  vt-parity  a delivered screen did not match tmux ground truth" >&2
  status=1
fi

# S9 gates. Every one of these is a real disagreement between the engine and
# tmux when it fails: the lane prints what it found and does not pass.
jq -r '
  .alacritty.quiescePoints | to_entries[] |
  "alacritty \(.key): \(if .value.pass then "PASS" else "FAIL" end)" +
  " text=\(.value.textMismatchRows)" +
  " attributes=\(.value.attributeMismatchCells)" +
  " anchors=\(.value.anchorMismatchRows)"
' "$runtime/vt-parity.json"
jq -r '
  .alacritty.fixtureClasses | to_entries[] |
  "class \(.key): \(if .value.pass then "PASS" else "FAIL" end)" +
  " alacritty \(.value.alacrittyPass)/\(.value.comparisons)" +
  " vt100 \(.value.vt100Pass)/\(.value.comparisons)"
' "$runtime/vt-parity.json"
jq -r '
  .alacritty.wrapCheck |
  "wrap: \(if .pass then "PASS" else "FAIL" end)" +
  " continuations \(.continuationsMarked // 0)/\(.continuationsExpected // 0)" +
  " copy-joined=\(.copyJoinsWrappedRows // false)" +
  " widths=\([.widthChecks[]? | "\(.columns):\(.rowsMatchLive and .copyMatchesLive)"] | join(","))"
' "$runtime/vt-parity.json"

if jq -e '
  (.alacritty.quiescePoints | to_entries | length == 5 and all(.value.pass))
  and (.alacritty.fixtureClasses | length > 0 and (to_entries | all(.value.pass)))
' "$runtime/vt-parity.json" >/dev/null; then
  printf 'PASS  vt-parity-alacritty  the engine matched tmux at all five quiesce points\n'
else
  echo "FAIL  vt-parity-alacritty  the engine disagreed with tmux; see .alacritty.divergences" >&2
  status=1
fi
if jq -e '.alacritty.wrapCheck.pass' "$runtime/vt-parity.json" >/dev/null; then
  printf 'PASS  vt-parity-wrap  composed history pages wrap, reflow and copy like live output\n'
else
  echo "FAIL  vt-parity-wrap  a composed history page did not behave like live output" >&2
  status=1
fi

# Separate from the vt100 gate above, which compares after another pane has
# printed: this one is about a session with nothing else printing.
if jq -e '.pauseRecoveredWithoutOtherOutput' "$runtime/vt-parity.json" >/dev/null; then
  printf 'PASS  vt-parity-pause-idle  paused panes recovered with no other output\n'
else
  echo "FAIL  vt-parity-pause-idle  paused panes had not both recovered 5 s after the flood was interrupted, with no other output" >&2
  status=1
fi

printf 'artifacts: %s\n' "$runtime"
exit "$status"
