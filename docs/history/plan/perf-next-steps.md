# Performance: next steps after `improvement/integration`

The Phase 15 campaign (tests/performance/benchmark/REPORT.md) confirmed one big win (file open
5.3 s → 3.1 s) and showed the frequent interactions were ack-gated, not
compute-bound. This file is the agreed follow-up plan: measure first, then a
series of small, independently shippable PRs. No more omnibus branches.

## Step 1 — Safety follow-ups (with the merge)

- Fix the terminal credit hang: a reader parked in `output_credit.rs` `reserve()`
  is only woken by an ack or the connection-wide `close()`
  (`terminal.rs:704`); the per-session detach path (`terminal.rs:690`) drops the
  client and joins its workers with nothing to wake a parked thread.
- Add the missing test suites: `TerminalWriteScheduler.ts` (dual byte
  accounting) and `git_content.rs` chunk streaming.
- Compile `src-tauri/src/perf_log/` out of release builds.

## Step 2 — Measure before optimizing (~1 day, no product code)

- Decompose file-open click→content into segments: control RTTs, scheduler
  admission, `BulkLease` acquisition, `OpenFileStream` RTT + transfer, Monaco
  fetch+eval, paint. The report's "the remaining ~3 s is network" is wrong at
  the campaign's actual ~27 ms median RTT; the residual is mostly local,
  removable serialization. Size every Step-3 item from this, not from guesses.
- Fix the pane-paint spans (`window.switch`, `create.*`) that the campaign found
  abandoned (0–1 samples), so perceived switch latency is measurable at all.
  `tmux.action.*` times only the host ack, not the paint.
- Fix the `click_point` coordinate bug (`tests/performance/benchmark/journey.py`) and re-run
  the short cached-revisit lane.

## Step 3 — User-facing perf PRs, re-prioritized from measurement

Step 2 ran; the numbers live in tests/performance/benchmark/decomposition.md. At ~30 ms RTT
a first-of-session file open spends ~490 ms in Monaco, ~310–410 ms in renderer
tab creation, ~245–325 ms in a fresh SSH lease, and ~32 ms on the wire. Every
big cost is first-open-of-session; steady-state opens are already fast.

1. **Idle-preload the Monaco chunk** (small, measured ~490 ms + gate). Fire the
   dynamic import from `requestIdleCallback` after the shell settles (pattern:
   `markdownPreview.ts:38`). Do NOT bundle into the entry chunk (1.0 MB → 5 MB,
   violates the tested bundle budget, taxes every terminal-only launch).
2. **Pre-warm one bulk lease at connect** (small, measured ~245–325 ms, grows
   with RTT). `leaseReuse` was false on every first open; keep the idle pool's
   floor at one lease per live connection.
3. **Fix the large-file open bug** — DONE and verified (`2bc9d2b`): the loop
   was the host echoing the client's own read back as a change event, on any
   file whose read outlived ~250-300 ms (~1 MB and up at 30 ms RTT — much
   broader than "large files"). All sizes now open first try; see
   tests/performance/benchmark/mac-verification.md.
4. **Decompose then trim renderer tab creation** (measure first; ~310–410 ms).
   Split `file.openIntent → dispatchToInvoke`: state update, lazy
   `AppTabSurface` chunk, mount. Optimize only what the split convicts.
5. **Paint on first chunk, not last** (medium). `OpenFileStream` buffers the
   whole file before first paint. Matters for medium/large files, not the 12 KB
   case; half of the fix for #3.
6. **Collapse the git-diff RTT staircase** (medium). Up to 20 sequential RTTs
   where an equivalent file costs 1; sketched in
   plan/wave2-integration-ledger.md R1.

Also opened by step 2: the once-observed settling-connection failure that
replaced a painted editor with an error card (decomposition.md, "Observed
once") — investigate before or with #2, since both touch lease/connection
lifecycle.

## Step 4 — Make switching feel instant (the daily-driver win)

Verified in code: the UI does not switch tabs until the host acks —
`shellNavigationCoordinator.ts` commits only on `"reached"` in `#finish`;
cross-workspace tab jumps await two sequential RTTs
(`useShellNavigation.ts:158`); new tab/workspace show nothing during the wait.
The machinery for instant switching already exists:

- `navigateLocal()` commits immediately (already used for app tabs and frozen
  mode).
- `TerminalStateCache` (20 panes) restores a previously-viewed pane locally on
  remount (`TerminalPane.tsx:258`).
- The snapshot snap-back guard pattern exists (`protectedAppTab` /
  `observeAuthoritativeWindow`).

PRs, in order:

1. **Optimistic tab & workspace switch** (small-medium). Commit locally on
   click via `navigateLocal`, repaint warm panes from cache, reconcile on ack;
   reuse the snap-back guard for terminal windows; stop sequencing
   selectSession → selectWindow as two awaited RTTs. Warm switches become
   0-RTT-perceived. Requires the Step-2 paint spans to prove the before/after.
2. **Pending-tab skeleton on create** (small). Insert a highlighted placeholder
   tab immediately on new tab / new workspace, upgrade on ack, roll back on
   failure. Full optimistic create with temp-id reconciliation is deliberately
   out of scope.
3. **Warm adjacent tabs in the background** (medium, later). Seed
   `TerminalStateCache` for recent/adjacent windows via the existing seed
   machinery so cold switches become warm ones.

Deliberately not planned yet: cold-start campaign (`run-ab.sh cold`) and
hover/selection prefetch — revisit both after Step 2's numbers exist.
