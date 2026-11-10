#!/usr/bin/env bash
# Checks docs/gpui-migration-ledger.md against the tracked desktop sources:
# every .ts/.tsx/.css file under apps/desktop/src has exactly one row, no row
# names an untracked file, and every retire row carries a reason.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
ledger=docs/gpui-migration-ledger.md
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git ls-files apps/desktop/src | grep -E '\.(ts|tsx|css)$' | sed 's#^apps/desktop/src/##' | sort > "$tmp/tracked"

# File rows live under "## Files": | `path` | lines | disposition | phase | target | status |
awk -F'|' '
  /^## Files/ { in_files = 1; next }
  in_files && $2 ~ /^ *`/ {
    path = $2; gsub(/^ *`|` *$/, "", path)
    disp = $4; gsub(/^ *| *$/, "", disp)
    target = $6; gsub(/^ *| *$/, "", target)
    print path > "'"$tmp"'/rows"
    if (disp != "port" && disp != "replace" && disp != "retire") print "bad disposition: " path " (" disp ")"
    if (disp == "retire" && length(target) < 10) print "retire without a reason: " path
  }
' "$ledger" > "$tmp/problems"

sort "$tmp/rows" > "$tmp/rows.sorted"
uniq -d "$tmp/rows.sorted" | sed 's/^/duplicate row: /' >> "$tmp/problems"
comm -23 "$tmp/tracked" <(uniq "$tmp/rows.sorted") | sed 's/^/missing row: /' >> "$tmp/problems"
comm -13 "$tmp/tracked" <(uniq "$tmp/rows.sorted") | sed 's/^/extra row: /' >> "$tmp/problems"

if [ -s "$tmp/problems" ]; then
  cat "$tmp/problems" >&2
  echo "migration ledger check failed" >&2
  exit 1
fi
echo "migration ledger ok: $(wc -l < "$tmp/tracked") files, $(grep -c '' "$tmp/rows") rows"
