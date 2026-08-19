#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"

evidence_dir="$repo_root/tests/release/evidence"
manifest="$evidence_dir/final-source-tree-manifest.tsv"
digest="$evidence_dir/final-source-tree-digest.txt"
work_root="$repo_root/tmp/work/final-source-evidence"
mkdir -p "$evidence_dir" "$work_root"
temporary="$work_root/manifest.tsv"

# The two generated outputs are evidence about the source set, not inputs to
# their own digest. Everything else in Git's tracked/unignored view is bound,
# including uncommitted source and durable QA screenshots/reports.
git ls-files --cached --others --exclude-standard -z \
  | LC_ALL=C sort -z \
  | while IFS= read -r -d '' path; do
      case "$path" in
        tests/release/evidence/final-source-tree-manifest.tsv|tests/release/evidence/final-source-tree-digest.txt)
          continue
          ;;
      esac
      hash=$(sha256sum -- "$path" | cut -d ' ' -f1)
      printf '%s\t%q\n' "$hash" "$path"
    done >"$temporary"

mv "$temporary" "$manifest"
sha256sum "$manifest" | cut -d ' ' -f1 >"$digest"
printf 'source tree digest: %s\n' "$(<"$digest")"
