# Tests

Tests are organized by what they validate:

- `integration/` exercises product domains such as transport, files, Git,
  agents, and transfers.
- `acceptance/` contains end-to-end Linux and macOS checks.
- `release/` validates packaging, reproducibility, installation, and rollback.
- `performance/` contains runtime and benchmark harnesses.
- `fixtures/` contains small, deterministic inputs committed with the tests.

Unit tests stay next to the Rust and TypeScript source they cover. The desktop
app's UI tests are in `apps/desktop/tests/` and in `#[cfg(test)]` modules under
`apps/desktop/src/`; they drive real views headlessly through GPUI's test
platform, and `cargo test -p muxflow` runs them. `apps/desktop/tests/frames.rs`
renders real frames offscreen, so it needs a Vulkan driver (lavapipe works) but
no display. Root commands
use the same vocabulary, for example `pnpm test:files`, `pnpm test:transfers`,
`pnpm test:release`, and `pnpm test:performance`.

Generated screenshots, logs, measurements, and raw QA results do not belong in
Git. Write them beneath `tmp/evidence/`. Test-local `evidence/` directories are
ignored to prevent accidental commits.
