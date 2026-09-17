# P3 branch ledger — precise filesystem watch propagation

Branch: `improve/p3-filesystem-explorer`
Base: `0faf78c` (`improvement/integration` — the accepted Wave 1 foundation)
Worktree: `/home/operator/dev/wave2-p3`
Not merged anywhere. This ledger records what the branch does, what it does not
do, how it was reviewed, and what was measured.

## Planned-item disposition

Every bullet of the plan's "P3 — precise filesystem watch propagation" section,
in the plan's order.

| Bullet | Status | Where |
| --- | --- | --- |
| Preserve mapped metadata/delete events and authoritative listings through the adapter instead of reducing them to invalidation | Done | `apps/desktop/src/features/files/api.ts` (`publishWireEvent`), `types.ts` (`WorkspaceEvent.directorySnapshot { listing }`, `fileChanged { entry? }`) |
| Patch complete cached listings for precise events; replace from authoritative snapshots; one recovery list only for overflow, gaps, incomplete pages, or mapping uncertainty | Done | `directoryState.ts` (`installListing`, `patchListing`, `oweRecovery`), `listingModel.ts` (`patchEntry`, `removeEntry`), `useWorkspaceFiles.ts` (`applyPrecise`, `coalesceRecovery`) |
| Keyed lease map diffing desired reachable expanded directories; release invisible descendant watches while retaining expansion memory | Done | `watchLeases.ts`, `listingModel.ts` (`reachableWatchTargets`) |
| Polling fallback per failed target with backoff/re-registration, healthy natives idle, 400 ms unchanged-snapshot loop removed | Done | `apps/host/src/service/filesystem/watch_fallback.rs`, `watch_service.rs` (`spawn_polling_fallback`, `fallback_watches`, `degrade_all_to_polling`) |
| Memoized stable row handlers; windowing *if* the instrumented 4,096-entry lane exceeds 150 ms or creates a long task | Done, with the conditional's own metric **not collected** — see Deferrals | `ExplorerRow.tsx`, `explorerWindow.ts`, `ExplorerTree.tsx` |
| Watch bootstrap as the initial remote listing; cache complete listings by connection/root/generation; paint a valid cached revisit and revalidate behind it; bounded prefetch cancelled on connection/root/generation change | Done | `api.ts` (`acquireDirectoryWatch`), `directoryCache.ts`, `useWorkspaceFiles.ts` (`prefetchNextPage`, `cacheKey`) |
| Start remote file I/O before editor evaluation; lightweight loading shell; one owning cancellation token across control and bulk phases; no retransmission of metadata already supplied | **Partly done. The loading shell, the one owning cancellation, and the no-retransmission halves are done; "before editor chunk evaluation" is UNMET on this branch — see Deferral 8.** | `AppTabSurface.tsx` (`load` starts on mount, `EmptyTab` shell), `file_stream.rs` (`CancelState` spans control + bulk), `FileStreamHeader` carries metadata once |
| One bulk `OpenFileStream` replacing metadata/preflight/per-chunk pull; one descriptor-bound generation; metadata/classification once; a warm open costs one RTT plus transfer; identity/epoch/root/path/generation/cancellation/terminal response bound before the bridge returns to the pool | Done | `crates/protocol/proto/envelope.proto`, `apps/host/src/service/filesystem/open_stream.rs`, `apps/host/src/service/requests/file_stream_dispatch.rs`, `apps/desktop/src-tauri/src/connection/files/{bulk_protocol,file_stream}.rs`. Content is buffered for classification rather than piped — see Deferrals. |
| Start initial read and parent-watch bootstrap together after subscribing; use the bootstrap generation to accept the first read or reload only on a real mismatch; never unconditionally re-read | Done | `AppTabSurface.tsx` (`reconcileBootstrap`, `listingOpinion`) |
| Snapshot-backed pagination with opaque tokens bound to server/root/directory identity and generation; later pages reuse the ordered snapshot | Done | `apps/host/src/service/filesystem/listing_page.rs`, `listing.rs` |
| Real cancellation from renderer operation ID through Tauri request ID to bounded host loops; mutations keep non-replay semantics | Done | `apps/desktop/src-tauri/src/connection/operations.rs`, `connection.rs`, `connection/files.rs` (`cancel_file_request`, cancellable only for `ListDirectory | WatchDirectory`), host `cancellation` checks in `listing.rs` / `open_stream.rs` |
| Replace the unconditional two-second active-root pipeline with a foreground backstop and a narrow generation-checked pane-CWD probe; unchanged roots emit no duplicate payload; replacement invalidates every cache | Partly done — see Deferrals | `useWorkspaceFiles.ts` (`ACTIVE_ROOT_BACKSTOP_MS`, `ACTIVE_ROOT_SETTLED_MULTIPLIER`), `active_root_dispatch.rs` (`known_root_token` fast path, suppressed duplicate broadcast), `directoryCache.ts` (`invalidateOtherRoots`) |

## Protocol schema additions

All *schema* additions are append-only and confined to the file lane. No Git
field, and no existing field's meaning, was changed.

**One operation was removed as a working operation, which is not an addition
and is the one thing on this list a merge agent must not miss:**

- `Operation::ReadFile` (`OPERATION_READ_FILE`) is still a defined enum value —
  the numbering is untouched — but the host now answers every `ReadFile`
  request with a refusal (`open_file_stream_required`,
  `apps/host/src/service/requests/filesystem_dispatch.rs`). `OpenFileStream` on
  the bulk lane is the only way to open a file, deliberately: the
  metadata + preflight + per-chunk staircase was a second code path for the
  same resource and had already drifted from this one. Anything outside this
  repository that still issues `ReadFile` will break, and there is no fallback.
  The in-repo caller was `tests/integration/filesystem/protocol-driver`, which is updated on
  this branch and passes — see Verification.

For the merge agent:

`crates/protocol/proto/envelope.proto`

- `Envelope.payload` gains `FileStreamFrame file_stream = 20;`
- `Operation` gains `OPERATION_OPEN_FILE_STREAM = 44;`
- new message `FileStreamFrame { string operation_id = 1; FileStreamHeader header = 2; uint64 offset = 3; bytes data = 4; bool eof = 5; string blake3 = 6; }`
- new message `FileStreamHeader { FileMetadata metadata = 1; FileContentKind content_kind = 2; uint64 generation = 3; uint64 total_bytes = 4; bool content_streaming = 5; }`
- `FileServiceRequest` gains `string known_root_token = 28;`
- `FileServiceResponse` gains `bool root_unchanged = 10;`
- `DirectorySnapshot` gains `bool recovered_from_overflow = 10;` (`overflowed` is re-documented as "more pages follow", its existing meaning)

`crates/protocol/src/lib.rs`

- `pub const CAP_FILE_STREAM: u64 = 1 << 15;`, included in `HOST_CAPABILITIES`.
  It is a *required* capability, not a negotiated one: the desktop's control
  handshake refuses a host whose advertised bits do not cover
  `HOST_CAPABILITIES`, and names the missing bit. There is no fallback path.

## Review

Six rounds. Every round used a fresh, independent reviewer subagent on opus,
each instructed to first read
`/home/operator/dev/worktree-cli/skills/thermo-nuclear-code-quality-review/SKILL.md`
in full and then review the complete branch diff from `0faf78c`. No round's
findings were passed into another round, and no reviewer was told what any
previous round found or what had been changed in response. **Every round,
including the sixth, returned BLOCK.** The findings below are the ones that
changed the branch, and the ones argued with are recorded as such rather than
quietly dropped.

Round 6 was the last one this cycle runs, so its findings were fixed and the
gates re-run, but nothing re-reviewed the result. What round 6 raised and this
branch did not act on is listed under "Round 6 residue" below — a merge agent
should read that section as the branch's known, unreviewed remainder rather
than assume the tree is clean.

The defects worth naming, because they were real and none of them were visible
from the code's own claims:

- **`dup` shares a file offset.** `directory_entry_names` handed `fdopendir` a
  duplicate of the capability's descriptor, so the *second* enumeration of any
  long-lived capability began where the first stopped — at the end. Every
  authoritative rescan of an already-listed directory reported it empty, and
  the desktop installs an authoritative listing as the directory's contents.
  Found by writing a test that publishes the same listing twice.
- **Patching required a complete listing.** The host's page is 4,096 entries,
  so in any larger directory every single-file change fell through to a
  recovery list and then a full re-pagination — the list storm this package
  exists to remove, in the directories where it costs most. The 4,096-entry
  evidence lane could not see it; a second lane at 8,192 now does.
- **A cancel could be written before its request.** Binding an operation to its
  request ID and writing that request were two steps, so a cancellation raised
  between them reached a host that discards a cancel for a request it has never
  seen. That is the window the package's cancellation guarantee lives in. The
  *first* fix serialized both behind one `dispatch` mutex held across the
  physical write, which put every Explorer cancellation in the queue behind any
  five-minute Git request and coupled the two lanes `OperationLane` exists to
  keep apart. The mutex is gone: the registry records a cancellation whether or
  not the request has an ID yet, and the dispatcher re-reads it on the far side
  of its write and re-sends. A duplicate cancel is harmless; a lost one is the
  whole guarantee.
- **Roving focus was a position.** Precise patching is what this branch made
  cheap, and it moves rows: an agent creating one file above the cursor carried
  the keyboard to a different file, and deleting the focused row dropped DOM
  focus to the document body. Both halves are fixed; the second took two
  attempts. Asking `treeRef.contains(document.activeElement)` *after* the
  removal is `false` in exactly the case it was written for — removing a focused
  element moves focus to `<body>` — so the restore was dead code, and the test
  named for it used `react-test-renderer` with no `createNodeMock`, leaving
  `treeRef.current` null so `.focus()` could not be observed at all. Ownership
  is now tracked as focus moves, and the test records `.focus()` through a node
  mock.
- **A shared watch's bootstrap was treated as every subscriber's own answer**,
  and one subscriber's abort cancelled the watch out from under the others.
- **Cancel-and-drain was installed in the shared bulk transport** for one
  operation's benefit, and turned cancellation of an `Inline` download or
  upload from milliseconds into the inactivity deadline.
- **A literal NUL byte in `directoryRequests.ts`.** `` `${path}<NUL>${kind}` ``
  as a raw `0x00` rather than the `\0` escape used everywhere else, which made
  git classify the file as binary: `git diff --numstat` reported `-  -` and
  `git diff` emitted `Bin 0 -> 3376 bytes`. The runtime behaviour was correct
  and the review integrity was not — the owner of every Explorer read's
  supersession serial, abort controller and teardown had no reviewable diff, no
  line-level merge, and no `git log -p` history, through four review rounds.
- **Cancel tombstones were evicted in bulk, across both lanes, on a budget that
  counted live claims.** Three defects in one sweep: it threw away exactly the
  tombstones whose claims were still crossing the command boundary (the entire
  case the mechanism exists for), sixty-four concurrent operations disabled
  eviction altogether, and a Git cancellation could evict a file read's refusal,
  after which that read dispatched to a host nobody would tell to stop.
  Eviction is now per lane, oldest-first, and counts only tombstones.
- **A precise change could be dropped from a paginated listing.**
  `listingModel.ts` decided whether a create belonged inside an incomplete
  listing's page by comparing mapped names with JavaScript's `<`, which is
  UTF-16 code-unit order; the host ranks on raw name bytes, and UTF-8 byte order
  is code *point* order. For an astral-plane name near a page boundary the two
  disagreed, `patchEntry` returned the listing unchanged, and nothing scheduled
  a recovery — the row was silently missing until the next authoritative
  snapshot. Comparison is by code point now, and a name carrying U+FFFD (whose
  raw bytes the renderer never sees) reports `"unmappable"` rather than
  guessing.
- **A page restore could swallow a genuine gap.** `installListing` overwrote a
  pending `{kind:"list"}` recovery with `{kind:"restorePages"}`, contradicting
  `oweRecovery`'s documented invariant — and `restorePages` returns silently on
  failure or abort without re-queuing, so an unmappable event answered by a
  failed restore was answered by nothing.
- **A fallback directory scan held the schedule lock the async poller uses.**
  `metadata`, `read_dir`, and up to 2,048 `symlink_metadata` calls ran holding
  the same `Mutex<FallbackTarget>` that `scan_due`, `native_retry_due`,
  `fallback_watches` and `degrade_all_to_polling` take from tokio tasks. The
  scan now has its own mutex and the schedule is only ever locked for field
  reads.

Arguments made against reviewer findings, with the reason:

- *"The cache never holds a directory over one page."* Correct, and intended:
  the plan says "cache **complete** listings". A partial listing painted from
  cache would show a truncated directory as though it were the whole one.
- *"`WireFileIoEvent.blake3` is dead."* It is not — the *write* path still
  publishes it (`editor_manager.rs`), and the field is read by `mapDownloadEvent`.
- *"Cache and watch keys omit `terminalEpoch`."* Deliberate; see the deferrals.
- *"`fallback_filesystem_scan_never_blocks_the_async_control_worker` does not
  test its name."* **Withdrawn — the objection was right in the form that
  matters.** The rebuttal above answered a weaker claim than the one that
  holds: the test proves the *scan* runs off the runtime thread and exercises
  none of the async-side locks, and the scan really did hold the same mutex
  `scan_due`, `native_retry_due`, `fallback_watches` and `degrade_all_to_polling`
  take from async tasks. The scan now lives behind its own mutex
  (`FallbackTarget::scan`), the schedule is only ever locked for field reads,
  and `the_async_poller_never_waits_on_a_scan_in_progress` asks every one of
  those questions while a scan holds its lock.

## Round 6 residue — what a merge agent inherits

Round 6 returned BLOCK. Everything on its "what I would require before
approval" list was verified against the source and fixed, and the gates were
re-run green afterwards. **No review has seen those fixes**, because round 6
was the last round this cycle runs.

Fixed in response to round 6, each verified against the code first:

| Finding | Disposition |
|---|---|
| A successful listing for a directory collapsed while it was in flight never cleared `loading`, so `aria-busy` stayed set for the life of the scope | Fixed. The wait state is settled by `applyListing` before the transition, because every exit owes it. |
| Collapse did not stop a coalesced recovery still inside its 150 ms window, so "collapse is one unwatch" was sometimes one unwatch and one list | Fixed. `abortListing` cancels the pending timers as well as the in-flight controllers. Regression test. |
| Every Explorer expand *and collapse* issued a `resolveActiveRoot`, which forks `tmux` on the host — new on this branch | Fixed. Activity takes the backstop off its settled interval and issues nothing; a host announcement and an explicit refresh still probe. |
| A rescan that could not publish re-armed its own flag, making the watcher a ~10 Hz re-list of every watch on the connection with no backoff | Fixed. Consecutive failed sweeps back off 250 ms → 8 s; a real watcher event still wakes it immediately. |
| The polling fallback committed its fingerprint before publishing and discarded the publish result, so a change could be lost permanently | Fixed. `publish_owed` survives a failed publish; a restore whose publish failed goes back on polling. |
| `restored_to_native` deferred its scan reset to a flag `advance_target` returns before consuming, pinning an open `ReadDir` for the life of the watch | Fixed. Released immediately whenever the scan is idle. |
| `PageBinding::capture` forked `tmux` on every listing *and* every page, including pages answered from memory | Fixed. The field was redundant with `root_token`, which already digests the server identity. |
| One entry vanishing between `readdir` and its stat failed the whole listing | Fixed, via one shared `entry_vanished` rule. |
| The bulk `Cancel` was an unchecked `write` on a non-blocking pipe | Fixed: bounded `poll` loop, lane marked unclean before the attempt. |
| The bulk handshake never applied the capability admission rule, on the only lane carrying `OpenFileStream` | Fixed. |
| A stale `capability_names` in `diagnostics.rs`, missing two bits | Deleted in favour of the canonical one. |
| `ReadFile` was `Lane::Control`, so the desktop its refusal exists for — which issued it on the bulk lane — never saw that refusal | Fixed: `Lane::Either`, `Scheduling::Inline`. |
| The `rootGeneration` cache-key comment claimed an invariant the host cannot provide | Comment corrected; the field is named as redundant rather than load-bearing. |
| The `explorerPaginatedWatchTraffic` lane never entered the incomplete-listing path it is named for | Fixed: three pages instead of two, and the extractor gates `heldListingComplete === false`. |

Raised by round 6 and **not** acted on. None is a correctness defect this
branch introduced; all are recorded so the next reader does not have to
rediscover them:

- **Structural restructurings (S1–S10).** Deriving `loading` from
  `DirectoryRequests` rather than mirroring it in React state; one `Operation`
  value replacing eleven hand-written "is this still current" expressions;
  moving the host's entry-ordering key onto the wire so `compareEntries`,
  `orderIsKnowable` and the `"unmappable"` recovery reason can be deleted
  outright; a `BatchAction` for the watcher's routing chain; list-then-register
  in `watch_directory_cancellable` to delete its rollback branch; a
  `run_file_op` helper for `filesystem_dispatch`'s seventeen-arm match. All are
  real and several would make a fixed defect structurally impossible, but each
  is a refactor of code this round is otherwise finished with, and none can be
  reviewed before the cycle ends.
- **Two residual host timers** (`watch_service.rs`: a 100 ms `select!` arm, and
  the fallback poller's 500 ms `IDLE_PARK`, which clones the whole watch
  registry each time). Both exist only to observe shutdown; a `Notify` would
  take them to zero. They are pre-existing and count against the round's idle
  budget.
- **Snapshot expiry can silently produce missing rows** (`listing.rs`): when a
  retained snapshot is gone the code rescans from `resume_after` against the
  *current* directory, and `recovered_from_overflow` is hardcoded `false`, so
  the client is never told the assembled listing may be inconsistent. Also, the
  watcher's own rescans insert retained snapshots nobody will redeem, which can
  evict a client's live pagination snapshot from the eight slots.
- **No byte ceiling at the Rust trust boundary** for a streamed open: the host
  enforces 10 MiB / 25 MiB before buffering, but `FileReadStream` bounds itself
  only by the host-declared `total_bytes`.
- **`readdir` errors are indistinguishable from EOF** in `path_policy.rs`
  (`errno` is neither cleared nor read), so an I/O error mid-enumeration yields
  a short listing stamped `authoritative: true`. Pre-existing.
- **`editor_io.rs` takes `_expected_generation` and discards it**, so a file
  changed under the editor is clobbered on save. Pre-existing, and the natural
  place a reader of *this* change would expect the guarantee to close.
- **Precise events are broadcast process-wide** while authoritative snapshots
  are connection-scoped. Pre-existing, harmless in content, N× duplicate
  traffic between two connections on the same root.
- **`.lock().unwrap()` throughout the filesystem service**: one panic poisons a
  mutex and every later operation on that connection panics.
  `scheduler.rs` already uses `unwrap_or_else(PoisonError::into_inner)`.
- Smaller: `explorer.expandToPaint` is published and read by nothing (also
  Deferral 1); `DirectoryRequests.#serial` grows unbounded for the life of a
  scope; `OperationRegistry::claim` leaves `tombstoned_at` set on a claim it
  adopts; `patchEntry` replaces in place without re-sorting, which would matter
  if the host ever reported a kind change as one event.

## Verification

Focused gates, all from the worktree with artifacts under `tmp/`:

- `pnpm --filter @tmux-agent-ide/desktop check`
- `pnpm --filter @tmux-agent-ide/desktop test` (91 files, 804 tests)
- `cargo fmt --all -- --check`
- `CARGO_TARGET_DIR=tmp/target cargo test -p tmux-ide-host --bins`
- `TAURI_CONFIG='{"bundle":{"externalBin":[]}}' CARGO_TARGET_DIR=tmp/target cargo test -p tmux-agent-desktop`
- `CARGO_TARGET_DIR=tmp/target cargo test -p tmux-agent-protocol`
- `TAURI_CONFIG='{"bundle":{"externalBin":[]}}' CARGO_TARGET_DIR=tmp/target cargo clippy --workspace --all-targets -- -D warnings`
- `bash tests/performance/optimization/run-explorer-after.sh <label>`
- The phase-4 protocol driver's local lane. It is **outside the cargo
  workspace** (`Cargo.toml` excludes `tests/integration/filesystem/protocol-driver`), so
  `cargo test --workspace --all-targets` cannot see it and no other gate on
  this list runs it. Build it explicitly
  (`cargo build --manifest-path tests/integration/filesystem/protocol-driver/Cargo.toml`) and
  run the `local` mode against a built `tmux-ide-host` with a tmux session in a
  scratch git repo, as `tests/integration/filesystem/run-backend.sh` lines 36–56 do. The full
  `run-backend.sh` additionally needs Docker and a Debian-compatible helper
  build for its remote half, which was not run here.

`TAURI_CONFIG` is set because `tauri::generate_context!` resolves
`bundle.externalBin` at compile time against a binary this checkout does not
build; the override is a build-time shim for the test gate only and changes no
shipped configuration.

### The phase-4 lane, and what running it turned up

Two separate defects, one of them older than this branch:

1. **This branch's.** `Operation::ReadFile` became an unconditional refusal, and
   the driver issued it four times. The driver now opens files with
   `OpenFileStream` on the bulk lane — the same path the app uses — and asserts
   the same classification, editor ceiling, and preview boundary facts.
2. **The base's.** With that repaired, the lane still failed earlier, at
   `"directory snapshot did not retain collapsed .git"`. `.git` is in the host's
   `ALWAYS_HIDDEN` list, so no listing has ever reported it as a row; the
   assertion contradicted the host's own contract. **This was verified to fail
   identically at `0faf78c` itself** by extracting that commit
   (`git archive 0faf78c`), building its host and its driver, and running the
   same lane: `Error: "directory snapshot did not retain collapsed .git"`. The
   lane was already red on `improvement/integration` and nothing said so. The
   assertion now states the host's two actual rules — `.git` hidden outright,
   `node_modules` shown and never expandable.

With both fixed the local lane exits 0 and its `jq` gate passes:

```json
{"activeRootAtomic":true,"blake3Verified":true,"controlBodiesRejected":true,
 "dotfilesVisible":true,"editorMaxReadWriteBytes":10485760,"folderDownload":true,
 "gitCollapsed":true,"maxControlLatencyMs":7,"nonEmptyConfirmation":true,
 "oversizedPreviewMetadataOnly":true,"previewBoundaryBytes":26214400,
 "rootToken":true,"streamedDownloadBytes":21,"textRoundTrip":true}
```

## Deferrals

Each is a deliberate decision, recorded here rather than left implicit.

1. **The 150 ms expand-to-paint and 300 ms external-change-to-paint budgets were
   not measured against a browser.** Those are browser-runtime numbers and this package was
   implemented without launching the app, which the round's QA instruction
   requires. Windowing was therefore added on the conservative side of the
   plan's conditional rather than because the metric said so. What *is*
   measured deterministically is the DOM cost the budget is about: the
   `explorerWide` phase-14 lane mounts 46 rows for a 4,096-entry directory and
   the same 46 for a 16,384-entry one, which is the invariant that separates a
   windowed tree from an unwindowed one. Both paint spans are now published and
   *read* by the wide lane — nothing read them before, so a budget expressed in
   milliseconds had no sample anywhere in the repository — and their jsdom p95
   is recorded beside the request counts, clearly labelled, never gated.
   Two precisions on that, because the sentence above overstated itself in an
   earlier revision:
   - "read" is true of the *duration*, not of every key. The wide lane reads
     `workflow.explorer.directoryExpandPaint` and `explorer.externalChangeToPaint`;
     the ticket also publishes `explorer.expandToPaint`, which nothing in the
     repository reads, and the emitted metric field is `jsdomExpandToPaintP95Ms`.
   - The "46 rows" figure is a function of `explorerWindow.ts`'s
     `DEFAULT_VIEWPORT_HEIGHT = 480` fallback, because jsdom reports
     `clientHeight === 0`. It is not measured layout. What the lane proves is the
     *invariant* — 46 at 4,096 entries and 46 at 16,384 — not the number.
   - **No long-task metric was collected anywhere.** The plan's "no long task
     > 50 ms" outcome for the Explorer is unmeasured on this branch; there is no
     long-task counter in any Explorer lane, and jsdom could not produce one.
2. **The phase-14 Explorer lane is a request ledger, not remote round-trip
   evidence.** It counts what the renderer asks the host for through a fake
   client; it does not measure an SSH link. It is honest about which of the two
   it is, and `run-explorer-after.sh` is deliberately separate from the Stage 0
   baseline runner so the shared harness stays runnable against the preserved
   baseline.
2a. **There is no `before` Explorer evidence, and there cannot be from this
   branch.** The plan requires "`before` evidence from the instrumented baseline
   before merging any optimization" and titles the section "Required
   **after/before** outcomes". `tmp/phase14/` holds only `explorer-after*` runs.
   `tests/performance/optimization/run-before.sh` runs only `ExplorerTree.test.tsx` and
   `api.test.ts`, and the two decisive lanes cannot run against baseline source
   at all — `acquireDirectoryWatch` and `DirectoryWatchLease.fresh` do not exist
   there. **Every Explorer number on this branch is after-only.** What the
   after-only counts do establish is absolute rather than comparative: zero
   directory lists for an expansion, one watch per directory, one release per
   collapse. A baseline that issued a list per expansion cannot produce those
   numbers, but this branch does not hold the run that shows it.
2b. **The host-side pagination enumerate/stat count is unmeasured.** The plan's
   "pagination after cache establishment enumerates/stats at most one page plus
   a small constant" is a count of host filesystem syscalls. Phase 14 counts
   renderer `listDirectory` calls against a fake client with no filesystem on
   the other side, and `listing_page.rs` has no evidence lane anywhere. The
   snapshot-reuse design is in the code and reviewed; the number is not
   measured.
3. **`OpenFileStream` buffers the file it is classifying instead of piping it.**
   "Is this text?" is a question about the whole file — a NUL byte or an invalid
   UTF-8 sequence anywhere makes it binary — so a header cannot honestly declare
   a classification it has not finished checking. The cost is bounded per open
   by the 10 MiB text / 25 MiB image ceilings and across opens by the
   dispatcher's four permits. The plan's phrase "streams bounded chunks
   continuously" is met in the sense that matters for the round trip count (one
   request, one header, a continuous body, no per-mebibyte pull) and not in the
   sense of piping the read.
4. **No narrow pane-CWD probe was added.** The active-root backstop replaces the
   two-second pipeline, the host answers an unchanged root from the caller's own
   capability with no second discovery and no broadcast payload, and the host's
   own root announcement now re-arms the backstop instead of being discarded.
   What is *not* present is a separate cheap "has this pane's cwd moved?"
   request: the backstop's own probe is that request. A `cd` inside the focused
   pane is therefore detected within one backstop interval rather than within
   two seconds, which is a deliberate trade of latency for idle cost.
5. **The foreground backstop slows down rather than stopping, so the round's
   "zero periodic desktop-to-host requests at idle" is NOT met for a visible
   idle window.** Two plan items pull in opposite directions here: the P3 bullet
   asks for a "foreground/relevant-surface backstop", and the idle budget asks
   for nothing periodic after warmup. What the code actually does:
   - A **hidden** window issues nothing at all. The interval checks
     `document.visibilityState` and returns.
   - A **visible** window — *including one that is not focused*, because
     `visibilityState` is `"visible"` for a window sitting behind another —
     probes once per settled interval (15 s × 8 = 2 minutes) forever. Over the
     plan's `run-idle-steady.sh 60` visible idle window that is roughly three
     `resolveActiveRoot` requests against a stated budget of zero.
   An earlier revision of this ledger said "a hidden **or unfocused** window
   issues nothing at all". That was false; `document.hasFocus()` is not
   consulted. Gating on it was tried and reverted: jsdom reports
   `document.hasFocus() === false` unconditionally, so the change silently
   disabled the backstop across nine existing tests and could not be verified
   anywhere in this branch's harness. It belongs to whichever lane can run a
   real window. The residual — a visible idle window's periodic probe — is a
   knowing deviation from the idle budget, recorded here rather than argued
   away, and the reason it is not simply switched off is that `cd` inside the
   pane the user is already in is announced by nothing at all: a backstop that
   stops never notices it again for as long as nobody touches the Explorer or
   hides the window.
6. **The Git service's `lossless_stat_component` still panics on a stat
   component that does not fit.** The filesystem copy was fixed; the Git copy is
   `apps/host/src/service/git/path.rs` and belongs to P4, which this package is
   instructed not to touch.
7. **Cache and watch keys do not include `terminalEpoch`.** They key on the
   connection (`clientId`), the root capability token, and the root generation.
   A tmux epoch bump does not change what is on disk or which capability names
   it, so including it would drop valid cached listings on every terminal
   restart for no correctness gain. A replaced connection or root already
   invalidates everything.
8. **"Begin the host request before editor chunk evaluation" is UNMET.**
   `AppTabSurface.tsx` starts `load()` on mount, before anything editor-shaped
   renders — but `AppTabSurface.tsx` itself statically imports
   `@monaco-editor/react`, and `App.tsx` lazy-loads `AppTabSurface`. The
   Monaco-bearing chunk therefore has to be fetched *and evaluated* before
   `load()` can issue the read at all. Making that boundary lazy is P6's package
   and this one does not touch it. The data flow this package owns is in place
   and will start the request first the moment the chunk boundary moves; the
   plan's required outcome, as written, is not met on this branch. Recorded here
   as a deferral rather than as a "Done" row in the disposition table, which is
   what it used to be.
9. **The QA activity was deterministic lanes, not a launched app.** No
   packaged-app or `cua` run happened here; the round's final packaged QA stage
   is owned by a separate agent. Everything claimed in this ledger comes from
   the gates listed under Verification.
