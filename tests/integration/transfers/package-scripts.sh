#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
jq -e '
  .scripts["test:git"] == "bash tests/integration/git/run-backend.sh" and
  .scripts["test:agents"] == "bash tests/integration/agents/run-backend.sh" and
  .scripts["test:transfers"] == "bash tests/integration/transfers/run-backend.sh" and
  .scripts["test:transfers:local"] == "bash tests/integration/transfers/run-local-transfer.sh" and
  .scripts["test:transfers:parent-swap"] == "bash tests/integration/transfers/run-parent-swap.sh" and
  .scripts["test:transfers:ssh"] == "bash tests/integration/transfers/run-docker-ssh.sh"
' "$repo_root/package.json" >/dev/null
grep -q 'pnpm test:transfers' "$repo_root/.github/workflows/ci.yml"
grep -q 'pnpm test:release:matrix' "$repo_root/.github/workflows/transfer-release.yml"
grep -q 'run-docker-ssh.sh' "$repo_root/tests/release/run-matrix.sh"
printf '%s\n' 'Git, agent, transfer, package, and CI gates are registered'
