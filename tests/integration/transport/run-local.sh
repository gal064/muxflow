#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repo_root"

cargo fmt --all -- --check
cargo test --workspace --all-targets
pnpm check
pnpm test
pnpm build
cargo run --quiet --bin tmux-ide-host -- doctor
cargo run --quiet --bin tmux-ide-host -- discover
cargo run --quiet --bin tmux-ide-host -- phase0-lanes

