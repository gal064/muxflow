# Wave 2 follow-ups

What this file is: the asymmetries, half-converted mechanisms, and known
oddities Wave 2 knowingly left behind, collected in one place so they become
scheduled work instead of ledger archaeology. Every item cites where it is
recorded. Deliberately excluded: net-new product features that were never in
scope (hover prefetch, partial/viewport file loading, large-file viewer mode) —
those are Wave 3 candidates, not loose ends.

Priority key: **P1** fix before/with the next feature wave touching the area ·
**P2** schedule soon, real debt · **P3** opportunistic.

## 1. One bulk-body transport instead of three — P1

The single largest "we did X for read and not for write" item
(`wave2-integration-ledger.md` R1).

- **Reads** stream over `OpenFileStream`: one request, continuous bounded
  chunks, one RTT plus transfer.
- **Saves** still use the pre-Wave-2 chunked write staircase in
  `editor_manager.rs` — a large save pays per-chunk round trips that the read
  path just eliminated.
- **Git diff bodies** use their own third scheme (`GitDiffContentRequest`
  chunk pulls): a large diff costs up to ~20 sequential RTTs versus one for a
  same-sized file open.

Converge writes and Git bodies onto the streaming design, then delete the
staircase and the Git-specific chunker. Sub-items that belong to the same
package:

- `FileReadStream` has no byte ceiling of its own at the Rust trust boundary —
  it bounds itself only by the host-declared `total_bytes`
  (`p3-filesystem-explorer-ledger.md`, round-6 residue).
- The renderer has two hand-written bulk-`Channel` readers that should become
  one (`wave2-integration-ledger.md` R8).
- `OpenFileStream` buffers rather than pipes: first content paints only when
  the last chunk lands. Piping into the editor progressively is part of the
  transport convergence, distinct from the out-of-scope partial-open feature.

## 2. Protocol warts — P1

- **`Operation::ReadFile` is a live enum value answered only with a refusal**
  (`open_file_stream_required`). This is Wave 2's one non-append-only protocol
  change; anything outside this repo issuing `ReadFile` breaks with no
  fallback. Decide its end state: formally deprecate in the proto with a
  version note, or remove at the next protocol version bump. Recorded in
  `p3-filesystem-explorer-ledger.md` ("protocol change a consumer must not
  miss") and the integration ledger.
- **`rootGeneration` in the directory cache key is redundant** with
  `root_token` and is now documented as such rather than removed. Remove the
  field. (P3 round-6 finding D1.)

## 3. Duplicate mechanisms that outlived their merge — P2

- **Three per-connection job registries**: the lane-keyed
  `OperationRegistry`, the transfer scheduler's registries, and P4's
  `GitContentReads` as a third (`wave2-integration-ledger.md` R2). Fold Git
  content reads into the shared registry.
- **One shared-observation concept, three implementations**
  (`wave2-integration-ledger.md` R7): the repository store, the directory
  watch leases, and the diff observation each reimplement
  subscribe/refcount/teardown. The small half — one `useGitRepository` used by
  both Git consumers — was recommended to land before P6 and did not.
- **Coordinator cache state is mutated from outside the coordinator**
  (`wave2-integration-ledger.md` R5) — move the mutation inside or make the
  cache an explicit input.
- **`useSharedGitDiff` publishes three raw mutable refs** across its API
  boundary (`wave2-integration-ledger.md` R3).
- **Editor-generation reconciliation may be deletable** (~60 lines +
  a `PaintTicket` field) if the paint-measurement contract is reworked to
  "next `onReady` while a ticket is pending" — blocked on P4's measurement
  contract, so it needs one owner across both features
  (`wave2-integration-ledger.md` P6-R1).

## 4. Idle budget: "zero requests" is still not zero — P2

The plan's idle outcome ("zero periodic desktop-to-host requests after
warmup") remains unmet in three recorded ways:

- A **visible-but-unfocused window still probes** `resolveActiveRoot` every
  settled interval (15 s × 8, then 120 s). Gating on `document.hasFocus()`
  was attempted and reverted because jsdom returns `false` unconditionally and
  silently disabled the backstop in nine tests — the fix needs a test-seam,
  not a straight gate. (`p3-filesystem-explorer-ledger.md` Deferral 5.)
- **Two residual host timers** exist only to observe shutdown: the watcher's
  100 ms `select!` arm and the fallback poller's 500 ms `IDLE_PARK`, which
  clones the whole watch registry each tick. A `Notify` takes both to zero.
  (P3 round-6 residue.)
- **No narrow generation-checked pane-CWD probe** was built; the backstop
  still runs a full `discover_authoritative` per probe.
  (`p3-filesystem-explorer-ledger.md` Deferral 4.)

## 5. Correctness edges recorded but not fixed — P2

None introduced by Wave 2; all found by its reviews and left documented:

- **Snapshot expiry can silently produce missing rows**: when a retained
  pagination snapshot is gone, the host rescans from `resume_after` against
  the *current* directory and hardcodes `recovered_from_overflow: false`, so
  the client is never told the assembled listing may be inconsistent. Related:
  the watcher's own rescans insert retained snapshots nobody redeems, which
  can evict a client's live pagination snapshot from the eight slots. (P3
  round-6 residue.)
- **`readdir` errors are indistinguishable from EOF** in `path_policy.rs`
  (`errno` neither cleared nor read): an I/O error mid-enumeration yields a
  short listing stamped `authoritative: true`. Pre-existing. (P3 round-6
  residue.)
- **Precise filesystem events are broadcast process-wide** rather than routed
  to the connection that watches the directory. (P3 round-6 residue.)
- **`.lock().unwrap()` is the pervasive poisoning policy** in the new host
  filesystem/watch code; decide and apply one policy (propagate vs. abort).
  (P3 round-6 residue.)

Note: the round-6 residue's "save discards `_expected_generation`" item was
subsequently **fixed at integration** (merge round 2 enforces the leaf
generation on save); it is listed here only so nobody re-reports it.

## 6. Structural refactors queued by reviewers — P3

Real simplifications, each making a fixed defect structurally impossible, all
deferred because they land after the cycle's last review:

- P3 round-6 S-list: derive Explorer `loading` from `DirectoryRequests`
  instead of mirroring it in React state; one `Operation` value replacing
  eleven hand-written "is this still current" expressions; put the host's
  entry-ordering key on the wire so `compareEntries`/`orderIsKnowable`/the
  `"unmappable"` recovery reason can be deleted; a `BatchAction` for the
  watcher's routing chain; list-then-register in
  `watch_directory_cancellable` to delete its rollback branch; a `run_file_op`
  helper for `filesystem_dispatch`'s seventeen-arm match.
- `spawn_watcher` is one long function with many interacting flags
  (`wave2-integration-ledger.md` R4); watcher recovery scheduling and
  coordinator capability lifetime interact awkwardly (R9).
- `useOpenFileTab.ts` is ~400 lines / seven effects; splitting it further
  requires separating the read from the bootstrap reconciliation that decides
  about that read — a P3-owned data-flow decision (`wave2-integration-ledger.md`
  P6-R3).
- The smaller verified-but-unactioned set in R6 of the integration ledger.

## 7. Review debt — P2

Three sets of commits were never seen by any reviewer, in order of size:

1. P3's round-6 remediation (13 fixes) — flagged "unreviewed" in its ledger.
2. The integration round-2 remediation and merge-conflict resolutions after
   the final integration reviewers reported.
3. P6's round-2 remediation.

The next review cycle that touches these areas should start from these
commits rather than re-reviewing the whole wave.

## 8. Measurement and evidence gaps — P2

- **No shaped journey lane exists for the new paths.** Explorer expand, file
  open, and Git panel/diff/mutation are proven by request/process counts, not
  milliseconds. Building the shaped phase-14 journey driver converts the
  wave's headline claims into latency numbers (and is the "before" for any
  Wave 3 work on the same paths).
- **Renderer-side budgets never measured**: Explorer 150 ms expand-to-paint /
  300 ms change-to-paint, long tasks >50 ms, first/repeated editor paint, and
  the fallback-atlas observation. All instrumented; all need a running app
  (packaged macOS lane for the atlas/WKWebView rows).
- **Host-side pagination enumerate/stat syscall counts unmeasured**
  (`listing_page.rs` has no evidence lane).
- **Phase-4 driver's Docker/remote half not run** (needs a Debian-compatible
  helper build); only the local lane gates.
- **P4 has no branch ledger** — its disposition rows in the integration ledger
  were derived from source and commit messages. Backfill if P4's area gets a
  Wave 3 package.
- **Monaco `automaticLayout` still runs beside the custom layout observer**
  (P6 bullet 4, half done) — disabling it is gated on packaged-macOS
  resize/restore coverage. The font-gate/capability narrowing likewise awaits
  its separately-reviewed capability matrix (measurements are captured in the
  P6 ledger section).

## 9. Harness and documentation oddities — P3

- **VT parity fails environmentally** at both the Wave 1 base and the Wave 2
  head (`pauseContinue`, 3 text rows, 0 attribute cells; delivered screen ~2
  frames behind ground truth despite `quiesced: true`). Wave 1 recorded this
  lane green at capture time. Diagnose whether the fixture's 700 ms quiesce
  window is racy under current machine load or something in the environment
  drifted; the lane is currently not trustworthy as a gate on this machine.
- **The enforced perf lane refuses any untracked file**, including the
  user-owned `tests/acceptance/macos/evidence/`, so it must be run from a temp
  worktree. Teach it an allowlist for user evidence directories.
- **The plan document's envelope command is stale**: it says
  `ADE_PHASE12_FLOOD_SECONDS=10`, but the hardened harness rejects enforced
  runs under 60 s. Update `plan/thorough-improvement-round.md`.
- **Decorative `eslint-disable-next-line react-hooks/exhaustive-deps`
  comments** exist in three hooks, but the repo has no ESLint configuration.
  Either add the tool or drop the convention repo-wide
  (`wave2-integration-ledger.md` P6-R5).

## 10. File-size watch list — P3

From the orchestrator's closing sweep: `apps/host/src/diagnostics.rs` is over
the 1,000-line review threshold (1,264) — pre-existing, and Wave 2 shrank it
slightly rather than growing it. Six more production files sit in the
850–990 band and will cross on their next feature: `connection.rs` (989),
`upload_manager.rs` (958), `terminal/stream.rs` (953), `bridge.rs` (914),
`App.tsx` (911), `service/git.rs` (870). Whichever wave touches one next
should budget the split up front. The sweep found no TODO/FIXME markers, no
stray debug output (all metric prints are deliberate phase-14 fixture
emissions), and only justified, commented clippy allows in the diff.
