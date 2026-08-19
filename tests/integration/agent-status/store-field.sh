#!/usr/bin/env bash
#
# Read one field of the daemon's single agent record out of its persisted
# store, without assuming `jq` or `python` on the host under test.
#
# The store is deliberately read as a file rather than through the protocol:
# what a reconnecting desktop gets is built from exactly these bytes, so an
# assertion against them is an assertion about what the user would see.
set -euo pipefail

store=$1
field=$2

[[ -f "$store" ]] || { echo "no store at $store" >&2; exit 1; }

lifecycle_name() {
  case "$1" in
    1) echo working ;;
    2) echo blocked ;;
    3) echo idle ;;
    4) echo unknown ;;
    *) echo "unspecified($1)" ;;
  esac
}

raw() {
  # `grep -o` returns matches in the order they appear, so this is the *first*
  # occurrence — which `sed`'s greedy prefix was not, it returned the last.
  # These lanes build exactly one record, but a reader that silently means
  # "some record" is not one to assert a phase on.
  grep -o "\"$1\":[^,}]*" "$store" \
    | head -1 \
    | sed -e "s/^\"$1\"://" -e 's/^"//' -e 's/"$//'
}

case "$field" in
  lifecycle) lifecycle_name "$(raw lifecycle)" ;;
  *) raw "$field" ;;
esac
