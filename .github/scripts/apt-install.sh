#!/usr/bin/env bash
# A stalled mirror keeps apt alive without progress, so apt's own timeouts never
# fire. Bound each attempt and go again rather than wait out the step's limit.
set -uo pipefail
options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
for attempt in 1 2 3; do
  if sudo timeout 240 apt-get "${options[@]}" update --error-on=any \
    && sudo timeout 240 apt-get "${options[@]}" install -y "$@"; then
    exit 0
  fi
  echo "::warning::apt attempt $attempt did not finish"
  sudo dpkg --configure -a || true
  sleep 5
done
exit 1
