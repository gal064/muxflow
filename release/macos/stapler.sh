#!/usr/bin/env bash
# `xcrun stapler staple|validate FILE`, retried before it is believed.
#
#   release/macos/stapler.sh validate /path/to/Muxflow.app
#
# stapler asks Apple's ticket service about the file, and that answer can fail
# for a moment: one release saw `staple` validate its own ticket and a
# `validate` of the same unchanged app eight seconds later fail, with a later
# run of the same scripts passing. A missing ticket still fails every attempt.
# stapler explains a failure on stdout, so its output is kept, not discarded.
set -euo pipefail

action=${1:-}
file=${2:-}
[[ ($action == staple || $action == validate) && -n "$file" ]] || {
  echo "usage: stapler.sh staple|validate FILE" >&2
  exit 64
}

attempts=5
for ((attempt = 1; ; attempt++)); do
  if output=$(xcrun stapler "$action" "$file" 2>&1); then
    exit 0
  fi
  echo "stapler $action $(basename "$file") failed (attempt $attempt of $attempts):" >&2
  printf '%s\n' "$output" >&2
  ((attempt < attempts)) || exit 65
  sleep $((attempt * 15))
done
