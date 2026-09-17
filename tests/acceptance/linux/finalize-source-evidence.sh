#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo_root"

evidence_dir="$repo_root/tests/acceptance/linux/evidence"
manifest="$evidence_dir/final-source-tree-manifest.tsv"
digest="$evidence_dir/final-source-tree-digest.txt"
work_root="$repo_root/tmp/work/phase9-final-source-evidence"
mkdir -p "$evidence_dir" "$work_root"
temporary="$work_root/manifest.tsv"

# The generated manifest and digest describe the tracked/unignored source set;
# they cannot recursively include themselves.
git ls-files --cached --others --exclude-standard -z \
  | LC_ALL=C sort -z \
  | while IFS= read -r -d '' path; do
      case "$path" in
        tests/acceptance/linux/evidence/final-source-tree-manifest.tsv|tests/acceptance/linux/evidence/final-source-tree-digest.txt)
          continue
          ;;
      esac
      hash=$(sha256sum -- "$path" | cut -d ' ' -f1)
      printf '%s\t%q\n' "$hash" "$path"
    done >"$temporary"

mv "$temporary" "$manifest"
sha256sum "$manifest" | cut -d ' ' -f1 >"$digest"
rmdir "$work_root"
printf 'source tree digest: %s\n' "$(<"$digest")"
