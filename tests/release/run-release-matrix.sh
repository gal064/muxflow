#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"

# Deliberate release/nightly-only opt-in. Each implementation path moves an
# exact 5 GiB upload and download; the driver and desktop-manager paths together
# move about 20 GiB per local/SSH gate and can take 15–20 minutes on shaped SSH.
export ADE_PHASE8_EXACT_5GIB=1
export ADE_PHASE8_EXACT_5GIB_CONFIRM=release
exec bash tests/release/run-matrix.sh
