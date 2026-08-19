#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo_root"

# Removed wire names remain only where protobuf reservations prevent reuse and
# where the realistic schema-v1 fixture proves old persisted routes are dropped.
forbidden='ADE_OUTER_TMUX|NestedProvenance|nested_tmux|outer_pane_id|nested_provenance|reported_pane_id'
if rg -n "$forbidden" apps crates \
  --glob '!apps/host/src/service/agents/store.rs' \
  --glob '!crates/protocol/proto/envelope.proto'; then
  echo 'removed nested-tmux implementation identifier remains in current source' >&2
  exit 1
fi

rg -n 'reserved "nested_tmux", "outer_pane_id", "nested_provenance"' \
  crates/protocol/proto/envelope.proto >/dev/null
rg -n 'reserved "nested_provenance", "reported_pane_id"' \
  crates/protocol/proto/envelope.proto >/dev/null
rg -n '"schema_version": 1' apps/host/src/service/agents/store.rs >/dev/null
rg -n '"nested_provenance": 2' apps/host/src/service/agents/store.rs >/dev/null

test ! -e docs/nested-tmux.md
! rg -n 'WindowFallback|SessionFallback|windowFallback|sessionFallback|closest surviving destination' \
  apps crates README.md docs docs/history/plan/product-requirements.md docs/history/plan/technical-plan.md
rg -n 'Nested tmux is unsupported' README.md docs/history/plan/product-requirements.md >/dev/null
rg -n 'Nested tmux is outside V1 support' docs/history/plan/technical-plan.md >/dev/null
rg -n 'pub const PROTOCOL_MAJOR: u32 = 2;' crates/protocol/src/lib.rs >/dev/null
rg -n '^version = "0.2.0"$' crates/protocol/Cargo.toml >/dev/null

printf 'phase9 source scan: pass\n'
