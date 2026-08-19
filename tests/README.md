# Tests

Tests are organized by what they validate:

- `integration/` exercises product domains such as transport, files, Git,
  agents, and transfers.
- `acceptance/` contains end-to-end Linux and macOS checks.
- `release/` validates packaging, reproducibility, installation, and rollback.
- `performance/` contains runtime and benchmark harnesses.
- `fixtures/` contains small, deterministic inputs committed with the tests.

Unit tests stay next to the Rust and TypeScript source they cover. Root commands
use the same vocabulary, for example `pnpm test:files`, `pnpm test:transfers`,
`pnpm test:release`, and `pnpm test:performance`.

Generated screenshots, logs, measurements, and raw QA results do not belong in
Git. Write them beneath `tmp/evidence/` or upload them as CI artifacts. Test-local
`evidence/` directories are ignored to prevent accidental commits.
