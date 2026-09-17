#!/usr/bin/env bash
# Renders the plan's Phase 12 budget table from a harness run directory.
# Usage: budget-report.sh <tmp/phase12-perf-*>
set -euo pipefail

runtime="${1:?usage: budget-report.sh <run directory>}"
docker_status="$(cat "$runtime/docker-status.txt" 2>/dev/null || echo unknown)"

row() {
  # row <verdict> <lane> <metric> <measured> <budget>
  printf '%-4s  %-6s  %-38s  %-22s  %s\n' "$1" "$2" "$3" "$4" "$5"
}

verdict() {
  # verdict <measured> <bound> <comparison: le|ge>
  local measured="$1" bound="$2" comparison="$3"
  if [[ -z "$measured" || "$measured" == "null" ]]; then
    printf 'MISS'
    return
  fi
  if [[ "$comparison" == "le" ]]; then
    awk -v m="$measured" -v b="$bound" 'BEGIN { print (m <= b) ? "PASS" : "FAIL" }'
  else
    awk -v m="$measured" -v b="$bound" 'BEGIN { print (m >= b) ? "PASS" : "FAIL" }'
  fi
}

report_lane() {
  local lane="$1" app="$2" raw="$3"
  local echo_budget="$4" switch_budget="$5" create_budget="$6" resize_budget="$7"
  local typing_flood_budget="$8"
  [[ -f "$app" ]] || { row 'MISS' "$lane" 'entire lane' 'no results file' "$app"; return; }

  local echo_p95 switch_p95 create_window_p95 create_session_p95 resize_p95
  local flood_mbps flood_resync flood_seconds idle_frames typing_flood_p95
  local total_resync wide_resync gap
  echo_p95=$(jq -r '.keystrokeEchoMs.p95Ms' "$app")
  switch_p95=$(jq -r '.windowSwitchMs.p95Ms' "$app")
  create_window_p95=$(jq -r '.createWindowInteractiveMs.p95Ms' "$app")
  create_session_p95=$(jq -r '.createSessionInteractiveMs.p95Ms' "$app")
  resize_p95=$(jq -r '.resizeSettleMs.p95Ms' "$app")
  flood_mbps=$(jq -r '.floodMegabytesPerSecond' "$app")
  flood_resync=$(jq -r '.floodResyncEvents' "$app")
  # A row must name the measurement it reports: this lane's flood duration is a
  # knob, and the exit criteria ask for it at 60 s.
  flood_seconds=$(jq -r '.floodSeconds // 10' "$app")
  typing_flood_p95=$(jq -r '.typingDuringFloodMs.p95Ms' "$app")
  idle_frames=$(jq -r '.idleFramesIn8s' "$app")
  total_resync=$(jq -r '.totalResyncEvents' "$app")
  wide_resync=$(jq -r '.connectionWideResyncEvents' "$app")
  gap=$(jq -r '.sequenceGapObserved' "$app")

  row "$(verdict "$echo_p95" "$echo_budget" le)" "$lane" \
    'keystroke -> glyph echo p95' "${echo_p95} ms" "<= ${echo_budget} ms"
  row "$(verdict "$switch_p95" "$switch_budget" le)" "$lane" \
    'window switch (visible+interactive) p95' "${switch_p95} ms" "<= ${switch_budget} ms"
  row "$(verdict "$create_window_p95" "$create_budget" le)" "$lane" \
    'new tab -> pane interactive p95' "${create_window_p95} ms" "<= ${create_budget} ms"
  row "$(verdict "$create_session_p95" "$create_budget" le)" "$lane" \
    'new workspace -> pane interactive p95' "${create_session_p95} ms" "<= ${create_budget} ms"
  row "$(verdict "$resize_p95" "$resize_budget" le)" "$lane" \
    'resize settle p95' "${resize_p95} ms" "<= ${resize_budget} ms"
  if [[ "$lane" == local ]]; then
    row "$(verdict "$flood_mbps" 10 ge)" "$lane" \
      'sustained output throughput' "${flood_mbps} MB/s" '>= 10 MB/s'
  else
    row 'INFO' "$lane" 'sustained output throughput' "${flood_mbps} MB/s" 'link-bound'
  fi
  row "$(verdict "$flood_resync" 0 le)" "$lane" \
    "connection resyncs during ${flood_seconds} s flood" "$flood_resync" '== 0'
  row "$(verdict "$typing_flood_p95" "$typing_flood_budget" le)" "$lane" \
    'typing latency during flood p95' "${typing_flood_p95} ms" "<= ${typing_flood_budget} ms"
  row "$(verdict "$idle_frames" 0 le)" "$lane" \
    'idle host frames in 8 s' "$idle_frames" '== 0'
  # The desktop is not in this harness, so its periodic work is invisible here.
  row 'MISS' "$lane" 'idle steady-state round trips (whole app)' \
    'desktop poll not measured' '== 0'
  row "$(verdict "$wide_resync" 0 le)" "$lane" \
    'connection-wide resyncs across the run' "$wide_resync" '== 0'
  row 'INFO' "$lane" 'pane-scoped reseeds across the run' \
    "$((total_resync - wide_resync))" 'recovery, not a resync'
  if [[ "$gap" == "false" ]]; then
    row 'PASS' "$lane" 'event sequence gaps' 'none' 'none'
  else
    row 'FAIL' "$lane" 'event sequence gaps' "$gap" 'none'
  fi

  if [[ -f "$raw" ]]; then
    local raw_p95 overhead
    raw_p95=$(jq -r '.keystrokeEchoMs.p95Ms' "$raw")
    overhead=$(awk -v a="$echo_p95" -v b="$raw_p95" 'BEGIN { printf "%.3f", a - b }')
    row 'INFO' "$lane" 'raw ssh+tmux echo p95 (baseline)' "${raw_p95} ms" 'reference'
    row "$(verdict "$overhead" 10 le)" "$lane" \
      'app overhead vs raw ssh+tmux p95' "${overhead} ms" '<= 10 ms'
  else
    row 'MISS' "$lane" 'app overhead vs raw ssh+tmux p95' 'no baseline file' '<= 10 ms'
  fi
}

printf 'Phase 12 budget table -- %s\n\n' "$(basename "$runtime")"
printf '%-4s  %-6s  %-38s  %-22s  %s\n' 'VERD' 'LANE' 'METRIC' 'MEASURED' 'BUDGET'
printf -- '---------------------------------------------------------------------------------------------------------\n'

report_lane local "$runtime/local-app.json" "$runtime/local-raw.json" 35 100 300 100 35
if [[ "$docker_status" == "ran" ]]; then
  report_lane docker "$runtime/docker-app.json" "$runtime/docker-raw.json" 135 150 500 150 350
else
  row 'BLOCK' 'docker' 'entire shaped 100 ms lane' "$docker_status" 'plan requires lane B'
fi

row 'MISS' 'app' 'explorer expand 4,096 entries' 'not measured' '<= 150 ms render'
row 'MISS' 'app' 'external file change visible in tree' 'not measured' '<= 300 ms'

printf '\nNotes:\n'
printf '  * This harness drives the protocol against a live host. It contains no\n'
printf '    desktop, so every row above describes the host and the link only. Rows\n'
printf '    that live in the renderer are reported MISS here rather than borrowing a\n'
printf '    protocol-lane number that does not measure them.\n'
printf '  * Reconnect-after-stall is covered by tests/performance/runtime/run-stall.sh.\n'
