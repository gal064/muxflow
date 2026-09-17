#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo_root"

UV_CACHE_DIR=${UV_CACHE_DIR:-"$repo_root/tmp/uv-cache"} \
  uv run --python 3.12 --no-project --no-config \
  python tests/release/check-repo-privacy.py
