#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo_root"

# Same production desktop managers, two-worker scheduler, independent control
# lane, digest/publication/cancellation assertions, and Docker SSH transport as
# the release gate; only payload size differs for routine feedback.
ADE_PHASE7_LOCAL_BYTES=${ADE_PHASE7_LOCAL_BYTES:-16777216} \
  bash tests/integration/transfers/run-local-transfer.sh
ADE_PHASE7_SSH_BYTES=${ADE_PHASE7_SSH_BYTES:-67108864} \
  bash tests/integration/transfers/run-docker-ssh.sh
