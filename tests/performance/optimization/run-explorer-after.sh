#!/usr/bin/env bash
# Phase 14 Explorer request-ledger lane, for sources that have P3.
#
# Deliberately separate from `run-before.sh`: that script is the Stage 0
# baseline runner, and the preserved baseline predates these fixtures. Pointing
# it at tests that do not exist at the baseline commit would make the shared
# harness runnable only against the source it was changed for, which is exactly
# the evidence problem the Wave 1 ledger exists to prevent.
#
# The lanes it emits (`explorerWideWatchTraffic`, `explorerPaginatedWatchTraffic`)
# are optional in `extract-metrics.py` for the same reason — the preserved
# baseline cannot produce them — but they are *required here*, because a lane
# this runner does not demand is a lane that could be deleted without failing
# anything. Present, they must describe one watch bootstrap per directory, no
# directory list at all, one unwatch per collapse, exactly one patched row —
# named, not merely counted — for one external change, and a cached revisit
# painted before its revalidation answered.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
label="${1:-after}"
artifacts="$repo_root/tmp/phase14/explorer-$label"
if [[ -e "$artifacts" ]]; then
  printf '%s\n' "refusing to overwrite existing artifacts: $artifacts" >&2
  exit 2
fi
mkdir -p "$artifacts/logs"

export UV_CACHE_DIR="${ADE_PHASE14_UV_CACHE_DIR:-$repo_root/tmp/phase14/uv-cache}"

desktop_exec() {
  local tool="$1"
  shift
  if [[ -x "$repo_root/apps/desktop/node_modules/.bin/$tool" ]]; then
    (cd "$repo_root/apps/desktop" && "./node_modules/.bin/$tool" "$@")
  else
    pnpm --filter @tmux-agent-ide/desktop exec "$tool" "$@"
  fi
}

status=0
desktop_exec vitest run \
  src/features/files/ExplorerTree.test.tsx \
  src/features/files/api.test.ts \
  src/features/files/useWorkspaceFiles.test.tsx \
  >"$artifacts/logs/explorer-wide.log" 2>&1 || status=1

# Scoped to the lanes this run produces, so the artifact's own `valid` flag is
# the verdict on the run rather than on every lane the extractor knows about.
# Unscoped, it wrote `"valid": false` citing six lanes nobody asked for, and a
# merge agent reading the artifact instead of this script's stdout got the
# opposite answer from the same run. The exit code is honoured for the same
# reason: discarding it left the artifact as the only record of a failure.
uv run --no-project "$repo_root/tests/performance/optimization/extract-metrics.py" --scope=explorer \
  "$artifacts/explorer-metrics.json" "$artifacts/logs/explorer-wide.log" \
  >"$artifacts/logs/extract.log" 2>&1 || status=1

uv run --no-project - "$artifacts/explorer-metrics.json" <<'PY' || status=1
import json
import sys

report = json.loads(open(sys.argv[1], encoding="utf-8").read())
errors = list(report["validationErrors"])
if not report["valid"] and not errors:
    errors = ["the artifact is stamped invalid without naming a reason"]
lanes = {record.get("lane") for record in report["records"]}
# Every Explorer lane this source emits, including the paginated one: an
# optional lane absent from this set could be deleted outright and the runner
# would still exit 0, which is not evidence of anything.
required = {
    "explorerWide",
    "explorerListWatch",
    "explorerWideWatchTraffic",
    "explorerPaginatedWatchTraffic",
}
missing = sorted(required - lanes)
for record in report["records"]:
    if str(record.get("lane", "")).startswith("explorer"):
        print(json.dumps(record))
if missing:
    print(f"missing Explorer lanes: {missing}", file=sys.stderr)
if errors:
    print("\n".join(errors), file=sys.stderr)
sys.exit(1 if errors or missing else 0)
PY

printf '%s\n' "phase14 explorer artifacts: $artifacts"
exit "$status"
