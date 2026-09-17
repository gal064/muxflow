#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repo_root"

cargo fmt --all -- --check
cargo test --workspace --all-targets
pnpm check
pnpm test
pnpm build
cargo run --quiet --bin muxflow-host -- doctor
cargo run --quiet --bin muxflow-host -- discover
cargo run --quiet --bin muxflow-host -- phase0-lanes

