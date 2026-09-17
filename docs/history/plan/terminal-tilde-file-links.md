# Plan: Support `~/…` terminal file links

## Context

ADE already recognizes separator-bearing terminal paths and sends them to the pane-owning host for authoritative resolution. A `~/…` token is currently made clickable, but the host treats it as relative to the pane cwd (`<cwd>/~/…`), so activation fails even when the file exists. Make current-user home-relative paths work on local and SSH hosts without widening the pane's existing filesystem capability.

## Implementation

- In `apps/desktop/src/features/terminal/terminalFilePaths.ts`, define the tilde grammar explicitly: accept `~/…`, keep absolute and existing relative forms, and reject `~user/…` and other leading-tilde spellings so unsupported paths are not presented as actionable links. Keep the lexical diagnostic helper consistent by resolving `~/…` only when given an absolute authoritative host home.
- In `apps/host/src/service/requests/active_root_dispatch.rs`, resolve `~/…` against that helper process's absolute `HOME` before canonicalization. Read `HOME` only for home-relative candidates, return a clear error when it is absent or non-absolute, and keep ordinary absolute/relative paths independent of `HOME`.
- Preserve the existing post-canonicalization checks unchanged: the target must exist, be a regular file, and remain inside the pane's already-authorized active root. Resolve on the host so SSH paths use the remote account's home, not the desktop user's home.
- Do not add shell evaluation, environment-variable interpolation, globbing, named-user lookup, protocol fields/capability bits, or persisted-state changes.

## Interfaces

- No public or wire-level API changes. `FileServiceRequest.path` continues carrying the original terminal token.
- The renderer-only lexical helper may take an optional authoritative home path for diagnostics/tests; runtime opening remains host-authoritative.

## Verification

- Extend `terminalFilePaths.test.ts` to prove wrapped `~/file` tokens are linked and resolved with an explicit host home, while `~alice/file` and malformed tilde forms stay inert.
- Extend the host `terminal_file_tests` to cover successful `~/…` resolution, missing/non-absolute `HOME`, unchanged absolute/relative behavior, `~/../…` confinement, missing files, and directories.
- Must pass: focused frontend test (`pnpm --filter @muxflow/desktop test -- terminalFilePaths.test.ts`), focused host tests (`cargo test -p muxflow-host terminal_file_tests`), `cargo fmt --all -- --check`, `pnpm check`, and `git diff --check`.

## Development Cycle

- Create a focused fix branch from `main` before implementation; no worktree is required.
- After the automated gates pass, launch one fresh independent `vercel-code-review` reviewer with only the request, this plan, and the current diff. Triage findings proportionally, fix justified defects, rerun affected checks, and repeat with a fresh reviewer only while findings remain substantive, up to three rounds.
- Skip manual QA exactly as requested; do not substitute browser, GUI, or other manual activity.
- Commit the implementation, tests, review-driven fixes, and this plan after review settles.

## Assumptions

- Only current-user `~/…` expansion is in scope; bare `~` and `~user/…` remain unsupported.
- Existing active-root confinement is intentional, even when a valid home-relative file exists elsewhere on the same host.
- Automated tests and independent reviews are required; manual QA is explicitly waived.
