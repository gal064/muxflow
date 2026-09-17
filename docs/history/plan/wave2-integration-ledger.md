# Wave 2 integration ledger

Integration base: `0faf78cbfe77f3ae37612e01582d0e0018a69430` — the accepted
Wave 1 head recorded in `plan/wave1-integration-ledger.md`.
Branch: `improvement/integration`
Final head: `9418150` (updated to the ledger commit itself, which is the only
change after this line — the source at `9418150` is what every gate below ran
against)

Wave 2 is two packages: P3 (precise filesystem watch propagation) and P4
(shared Git observation and sidebar rendering). Both branched from the exact
same commit, so neither carries the other's work and every overlap below is a
genuine two-author collision rather than a rebase artefact.

## Package outcomes

| Order | Package | Reviewed/branch tip | Integration merge | Outcome |
| --- | --- | --- | --- | --- |
| 1 | P3 filesystem/Explorer/file-stream | `314a2e1` | `5fa1837` | Merged. No textual conflict — P3 branched from this base and merged first. Its branch ledger `plan/p3-filesystem-explorer-ledger.md` is tracked and came in with it. |
| 2 | P4 shared Git observation | `47f75d2` | `dcbbee6` | Merged with three conflicted files and one module dropped as redundant. See "Conflict decisions". |

Both branch tips are ancestors of the final head. Neither branch nor worktree
was deleted or rewritten, and `main` was not touched.

Integration commits on top of the two merges, in order:

| Commit | What |
| --- | --- |
| `bb9301b` | Removed a trailing blank line at EOF in `ExplorerTree.tsx` (P3) and `GitSidebar.tsx` (P4), which `git diff --check` flagged over the range. |
| `f024009` | Round-1 review remediation, both lanes. |
| `9c7acf4` | Round-2 review remediation, TypeScript lane. |
| `9418150` | Round-2 review remediation, Rust lane. |

## Branch-gate evidence the merge agent verified

The plan's mandatory thermo-nuclear branch gate requires each implementor to
have run independent reviewers before its work is eligible to merge. What was
retained for each branch, stated as retained rather than as verified-by-me:

| Branch | Retained branch gate | Final branch verdict | Unreviewed remainder at the tip |
| --- | --- | --- | --- |
| P3 | Six rounds, each a fresh independent opus subagent instructed to read `/home/operator/dev/worktree-cli/skills/thermo-nuclear-code-quality-review/SKILL.md` in full and review `0faf78c..tip`. Recorded in the branch ledger with the defects each round found. | **Every round, including the sixth, returned BLOCK.** Round 6's requirements were fixed and gates re-run. | The round-6 remediation (13 fixes) was never re-reviewed on the branch. Its own known residue is listed in the branch ledger's "Round 6 residue" section and is carried forward below. |
| P4 | Five rounds reported by the implementor. No branch ledger was written, so no per-round finding list survives in the repository. | Round 5 found a dead-descriptor watcher bug (the repository watch was registered against a `/proc/self/fd/N` path whose descriptor was already dropped, so every directory created after the watch went unwatched on Linux — the primary deployment path). Fixed in `47f75d2` with a regression test. | `47f75d2` itself, the round-5 remediation, is unreviewed. |

Both branches therefore arrived with an unreviewed tip. The integration-level
thermo-nuclear review below reviews the entire merged range `0faf78c..HEAD`,
which includes both unreviewed remediations, and is the only review those
commits have had.

## Conflict decisions

Three files conflicted textually. One further collision was semantic only and
git merged it cleanly without noticing.

### 1. `crates/protocol/proto/envelope.proto` — colliding operation number

Both branches independently claimed `= 44` for a new `Operation`:
P3's `OPERATION_OPEN_FILE_STREAM`, P4's `OPERATION_GIT_DIFF_CONTENT`.

**Decision.** `OPEN_FILE_STREAM` keeps 44; `GIT_DIFF_CONTENT` moves to 45.
P3 merged first, its number is asserted by a protocol test and referenced by
the `CAP_FILE_STREAM` handshake contract, and P4's Rust and TypeScript refer to
the operation only by name (`v1::Operation::GitDiffContent`), so renumbering it
is one proto line. Neither number has ever been released outside this
repository. `crates/protocol/tests/roundtrip.rs` now asserts the settled
assignment 43/44/45 and that 46 is still unknown, so neither can drift back.

No other collision exists in the schema: P3 adds `Envelope.payload` field 20
and P4 adds none; P3 extends `FileServiceRequest`/`FileServiceResponse`/
`DirectorySnapshot` and P4 extends `GitDiff`/`GitRequest`/`GitResponse`, which
are disjoint messages. A field-number audit of all 51 message and enum scopes
found no duplicate, and `protoc` compiled the merged schema.

### 2. `apps/desktop/src-tauri/src/connection.rs` — two cancellation registries

The base had `git_operations: Mutex<HashMap<String, u64>>`. Both branches
replaced it, for the same reason and with nearly the same design:

- P3 replaced it with `connection/operations.rs`: an `OperationRegistry` keyed
  by `(OperationLane, operation_id)` covering **both** the Git and file lanes,
  with claim-before-dispatch, cancel-before-claim tombstones bounded per lane
  oldest-first, a post-write re-read so a cancellation racing the physical
  write is re-sent, and an RAII `OperationClaim`.
- P4 replaced it with `connection/git_operations.rs`: a `GitOperations` map for
  the Git lane only, with register/complete/cancel/reset, cancel-before-register
  tombstones bounded at 256 with a 30 s TTL.

**Decision.** Keep P3's `OperationRegistry` and **delete
`connection/git_operations.rs` entirely.** Two registries for one concern, one
of them a strict subset of the other, is the spaghetti this round exists to
remove. P3's is lane-keyed, which P4's is not, and P3's ledger records a
specific defect that only the lane key prevents: a Git cancellation evicting a
file read's tombstone, after which that read dispatches to a host nobody will
tell to stop.

Nothing P4 needed is lost. Every behaviour P4's module tested is already
asserted against P3's registry:

| P4 `GitOperations` behaviour | Where it survives |
| --- | --- |
| Cancel before registration refuses the request exactly once | `connection/operations.rs::a_cancellation_that_arrives_before_the_claim_still_refuses_it`, and `connection/tests.rs::cancelling_an_operation_before_it_is_dispatched_refuses_it_rather_than_failing`, which runs both lanes |
| Cancelling an in-flight request returns its bridge request id | `operations.rs::a_cancellation_raised_after_dispatch_is_still_visible_to_the_dispatcher` |
| Duplicate registration refused without disturbing the first | `operations.rs::cancellation_targets_the_exact_lane_and_leaves_the_other_alone` |
| Tombstones bounded, never evicting a live request | `operations.rs::a_cancellation_that_arrives_before_the_claim_still_refuses_it` and `a_lane_cannot_evict_the_other_lanes_cancellations` |
| Reset forgets everything bound to the replaced bridge | `operations.rs::a_claim_whose_registry_was_cleared_is_refused_rather_than_dispatched` |
| `cancel_git` on an unknown id answers `Ok`, not `Err` | `connection/tests.rs::cancelling_an_operation_before_it_is_dispatched_refuses_it_rather_than_failing` — P3 pinned the exact behaviour change P4 also made |

One deliberate difference is recorded rather than hidden: P4's tombstones
expired after 30 s, P3's do not expire and are instead bounded at 64 per lane
and evicted oldest-first. Both are bounded; the surviving rule is P3's.

P4's `git_content_reads: Arc<GitContentReads>` field, its `git_content_reads()`
accessor, its `cancel_all()` in `fail_pending`, the `pub(crate) mod git_content`
declaration and the `pub(crate) struct TerminalClient` widening are all kept —
they are Git-diff-body ownership, not cancellation bookkeeping.

### 3. `apps/desktop/src-tauri/src/connection/files/bulk_protocol.rs` — `Exchange` vs the wrapper set

P3 deleted six `request_*` wrappers (`request`, `request_classified`,
`request_with_deadline`, `request_classified_with_deadline`,
`request_cancellable`, `request_classified_cancellable`) and replaced them with
one `Exchange<'a> { cancellation, deadline, on_frame }` struct, because the
wrappers named every *combination* of four optional parameters and the new
file-stream body-frame capability would have needed more of them. P4 widened
`request_cancellable` to `pub(crate)` so its Git diff-body lane could use it.

**Decision.** Keep P3's `Exchange`; the wrapper P4 widened no longer exists.
`Exchange`, `Exchange::live` and `BulkProtocolClient::request` are widened to
`pub(crate)` — the same widening P4 asked for, applied to the shape that
replaced its target. `connection::git_content` is a sibling of
`connection::files`, not a child, so `pub(super)` cannot reach it.

P4's one call site becomes:

```rust
protocol.request(request, Exchange::live(&job.cancellation, &deadline))
```

`on_frame` stays `None` for the Git lane on purpose: a diff body comes back in
its response, not in file-lane body frames, and `request_framed` treats an
unexpected `FileStream` payload as a transport failure. The Git lane therefore
cannot silently accept a frame it has no reader for.

### 4. `connection/operations.rs` visibility — a lint the merge created

P4 made `TerminalClient` `pub(crate)` so `git_content` could name it. That
promoted the reachability of P3's `pub(crate) fn claim_file_operation`, whose
return type `OperationClaim` was `pub(super)`, and `clippy -D warnings` failed
with `private_interfaces`. `OperationClaim` is now `pub(crate)`. It remains
opaque — no public constructor, no public field — so nothing is exposed beyond
the ability to hold one.

`cargo check` does not catch this; only the clippy gate does.

### 5. `files/bulk_pool.rs` `MAX_IDLE` 2 → 3 — merged cleanly, verified semantically

P4 raised the idle-bulk-connection bound from 2 to 3, reasoning "two for the
scheduler's concurrent-transfer bound plus one for the Git diff-body lane".
P3 added a third bulk consumer, `OpenFileStream`, after that comment was
written, so the arithmetic had to be re-checked rather than assumed.

It holds: `run_file_read` (the `OpenFileStream` path) is reached through
`FileIoManager::enqueue` → `enqueue_transfer_with_queued`, the same scheduler
whose two-active bound the comment already counts. Git diff bodies are bounded
separately at `MAX_CONCURRENT_READS = 1` per connection. Concurrent bulk
consumers are therefore exactly 2 + 1 = 3, and the comment is accurate in the
merged tree. No change was made.

### 6. Cross-branch item verified, no change needed

- **P3's capability-rule enforcement on the bulk handshake vs P4's Git bulk
  lane.** P3's round-6 fix C2 made `BulkProtocolClient::handshake` refuse a
  helper whose advertised capabilities do not cover `HOST_CAPABILITIES`, naming
  the missing bit. P4's diff-body lane acquires the same `BulkLease` and passes
  through the same handshake. The desktop's bulk `ClientHello` requests
  `HOST_CAPABILITIES` and the host answers
  `HOST_CAPABILITIES & requested_capabilities`, so a same-repository host
  negotiates the full set and the Git lane is admitted. P4 adds no capability
  bit of its own; it gates on the per-connection `bulk_available` flag instead.
- **Exhaustive operation policy.** The production match in
  `operation_policy.rs` is compiler-exhaustive and now carries both
  `OpenFileStream` (ReadOnly/Bulk/Detached/Filesystem) and `GitDiffContent`
  (ReadOnly/Bulk/Detached/Git). Its compatibility test was audited
  programmatically against the proto: 49 operations in the schema, 49
  `assert_policy` calls, no operation missing and none extra.

## Planned-item disposition — P3

Carried forward from `plan/p3-filesystem-explorer-ledger.md` without
relabelling. Nothing marked UNMET, unmeasured or partly-done there is presented
as done here.

| P3 plan bullet | Disposition | Where |
| --- | --- | --- |
| Preserve mapped metadata/delete events and authoritative listings through the adapter rather than reducing them to invalidation | Merged | `features/files/api.ts` (`publishWireEvent`), `types.ts` |
| Patch complete cached listings for precise events; replace from authoritative snapshots; one recovery list only for overflow/gaps/incomplete pages/mapping uncertainty | Merged | `directoryState.ts`, `listingModel.ts`, `useWorkspaceFiles.ts` |
| Keyed lease map diffing reachable expanded directories; release invisible descendant watches while retaining expansion memory | Merged | `watchLeases.ts`, `listingModel.ts` |
| Per-target polling fallback with backoff/re-registration; healthy natives idle; 400 ms unchanged-snapshot loop removed | Merged | `filesystem/watch_fallback.rs`, `watch_service.rs` |
| Memoized stable row handlers; windowing **if** the instrumented 4,096-entry lane exceeds 150 ms or creates a long task | **Merged, with the conditional's own metric NOT COLLECTED.** Windowing was added on the conservative side of the conditional, not because a measurement demanded it. | `ExplorerRow.tsx`, `explorerWindow.ts`, `ExplorerTree.tsx`; deferral 1 below |
| Watch bootstrap as the initial listing; cache complete listings by connection/root/generation; paint a valid cached revisit and revalidate behind it; bounded prefetch cancelled on scope change | Merged | `api.ts` (`acquireDirectoryWatch`), `directoryCache.ts`, `useWorkspaceFiles.ts` |
| Start remote file I/O before editor evaluation; loading shell; one owning cancellation across control and bulk; no retransmission of supplied metadata | **PARTLY MERGED.** Loading shell, single owning cancellation and no-retransmission are done. **"Before editor chunk evaluation" is UNMET** — see deferral 8. | `AppTabSurface.tsx`, `files/file_stream.rs`, `FileStreamHeader` |
| One bulk `OpenFileStream` replacing the metadata/preflight/per-chunk staircase; one descriptor-bound generation; a warm open is one RTT plus transfer; identity/epoch/root/path/generation/cancellation/terminal response bound before the bridge returns to the pool | Merged. Content is buffered for classification rather than piped — deferral 3. | `envelope.proto`, `filesystem/open_stream.rs`, `requests/file_stream_dispatch.rs`, `connection/files/{bulk_protocol,file_stream}.rs` |
| Start initial read and parent-watch bootstrap together after subscribing; use the bootstrap generation to accept the first read; never unconditionally re-read | Merged | `AppTabSurface.tsx` (`reconcileBootstrap`, `listingOpinion`) |
| Snapshot-backed pagination with opaque tokens bound to server/root/directory identity and generation | Merged in code. The host-side enumerate/stat count the plan states as an outcome is **UNMEASURED** — deferral 2b. | `filesystem/listing_page.rs`, `listing.rs` |
| Real cancellation from renderer operation id through Tauri request id to bounded host loops; mutations keep non-replay semantics | Merged | `connection/operations.rs`, `connection.rs`, `connection/files.rs`, host `listing.rs` / `open_stream.rs` |
| Replace the unconditional two-second active-root pipeline with a foreground backstop and a narrow generation-checked pane-CWD probe; unchanged roots emit no duplicate payload; replacement invalidates every cache | **PARTLY MERGED.** Backstop, unchanged-root fast path and cache invalidation are done. **No separate narrow pane-CWD probe was added** (deferral 4), and the backstop slows rather than stops, so a *visible* idle window still issues periodic probes (deferral 5). | `useWorkspaceFiles.ts`, `useActiveRoot.ts`, `requests/active_root_dispatch.rs`, `directoryCache.ts` |

### P3 protocol change a consumer must not miss

`Operation::ReadFile` keeps its enum number but is now an unconditional refusal
(`open_file_stream_required`, `requests/filesystem_dispatch.rs`).
`OpenFileStream` on the bulk lane is the only way to open a file, and there is
no fallback. The one in-repo caller, `tests/integration/filesystem/protocol-driver`, was
updated on the P3 branch. `CAP_FILE_STREAM = 1 << 15` is a *required*
capability, not a negotiated one: the control and bulk handshakes both refuse a
host whose advertised bits do not cover `HOST_CAPABILITIES` and name the
missing bit.

### P3 deferrals carried forward verbatim in substance

1. **The 150 ms expand-to-paint and 300 ms external-change-to-paint budgets
   were never measured against a browser**, and **no long-task metric was
   collected anywhere**. The deterministic invariant that *is* measured is the
   DOM cost: 46 mounted rows at 4,096 entries and 46 at 16,384. jsdom paint p95
   is published beside the request counts, labelled, never gated.
2. **The phase-14 Explorer lane is a renderer request ledger, not remote
   round-trip evidence.** It counts what the renderer asks a fake client for.
   2a. **There is no `before` Explorer evidence and there cannot be from this
   branch** — the two decisive lanes cannot run against baseline source at all.
   Every Explorer number is after-only and absolute, never comparative.
   2b. **The host-side pagination enumerate/stat count is unmeasured.**
3. **`OpenFileStream` buffers the file it classifies rather than piping it**,
   bounded by the 10 MiB text / 25 MiB image ceilings and four dispatcher
   permits.
4. **No narrow pane-CWD probe was added**; the backstop's own probe is that
   request, so a `cd` in the focused pane is noticed within one backstop
   interval rather than within two seconds.
5. **The round's "zero periodic desktop-to-host requests at idle" is NOT met
   for a visible idle window.** A hidden window issues nothing; a visible one —
   *including one that is not focused* — probes once per settled interval
   (15 s × 8 = 2 minutes) forever. Gating on `document.hasFocus()` was tried and
   reverted because jsdom reports it `false` unconditionally.
6. **`apps/host/src/service/git/path.rs::lossless_stat_component` still panics**
   on a stat component that does not fit. P3 fixed the filesystem copy and was
   instructed not to touch P4's. **P4 did not fix it either**, so it survives
   into the integration unchanged. Verified against the merged tree.
7. **Cache and watch keys deliberately exclude `terminalEpoch`.**
8. **"Begin the host request before editor chunk evaluation" is UNMET.**
   `AppTabSurface.tsx` starts `load()` on mount, but it statically imports
   `@monaco-editor/react` and `App.tsx` lazy-loads `AppTabSurface`, so the
   Monaco-bearing chunk must be fetched and evaluated before `load()` can issue
   the read. **This is P6's boundary to move.** See the handoff section.
9. **The QA activity was deterministic lanes, not a launched app.**

### P3 round-6 residue inherited unreviewed

Raised by P3's round 6 and deliberately not acted on. None is a defect P3
introduced; all are recorded so the next reader does not rediscover them.

- Structural restructurings S1–S10: deriving `loading` from `DirectoryRequests`
  rather than mirroring it in React state; one `Operation` value replacing
  eleven hand-written currency checks; moving the host's entry-ordering key onto
  the wire so `compareEntries`/`orderIsKnowable`/the `"unmappable"` recovery
  reason could be deleted; a `BatchAction` for the watcher routing chain;
  list-then-register in `watch_directory_cancellable`; a `run_file_op` helper
  for `filesystem_dispatch`'s seventeen-arm match.
- Two residual host timers (`watch_service.rs` 100 ms `select!` arm; the
  fallback poller's 500 ms `IDLE_PARK`, which clones the watch registry each
  time). Pre-existing; they count against the idle budget.
- `listing.rs` snapshot expiry can silently produce missing rows:
  `recovered_from_overflow` is hardcoded `false` on a rescan from `resume_after`
  against the current directory, and the watcher's own rescans can evict a
  client's live pagination snapshot from the eight slots.
- No byte ceiling at the Rust trust boundary for a streamed open:
  `FileReadStream` bounds itself only by the host-declared `total_bytes`.
- `path_policy.rs` cannot distinguish a `readdir` error from EOF, so an I/O
  error mid-enumeration yields a short listing stamped `authoritative: true`.
  Pre-existing.
- `editor_io.rs` takes `_expected_generation` and discards it, so a file changed
  under the editor is clobbered on save. Pre-existing.
- Precise events are broadcast process-wide while authoritative snapshots are
  connection-scoped. Pre-existing; duplicate traffic, not wrong content.
- `.lock().unwrap()` throughout the filesystem service: one panic poisons a
  mutex and every later operation on that connection panics.
- Smaller: `explorer.expandToPaint` is published and read by nothing;
  `DirectoryRequests.#serial` grows unbounded for a scope's life;
  `OperationRegistry::claim` leaves `tombstoned_at` set on a claim it adopts;
  `patchEntry` replaces in place without re-sorting.

## Planned-item disposition — P4

P4 wrote no branch ledger, so these rows were derived by the merge agent from
the merged source and the branch's commit messages, not copied from a report.
Where the plan states a *measured* outcome that no lane in this repository
collects, the row says so rather than inheriting the implementor's claim.

| P4 plan bullet | Disposition | Where |
| --- | --- | --- |
| Share one repository watch/status stream among sidebar and matching diff tabs; saved/cross-root tabs retain an explicit fallback | Merged | `features/git/repositoryStore.ts`, `useSharedGitDiff.ts`, `useWorkspaceGit.ts`; host `git/coordinator.rs` |
| Deduplicate worktree/git/common directory watch paths and multiplex subscribers so 32 consumers create one native watcher and one coalesced status refresh | Merged, with a deterministic fixture | `git/coordinator/{watcher,subscribers}.rs`; `git/tests/watch_reconnect.rs::phase14_thirty_two_consumers_report_native_watchers_and_status_processes` |
| Separate commit-form state from memoized status groups and equality-bail the same focused row so typing does not rebuild up to 1,000 rows | Merged | `features/git/GitCommitForm.tsx`, `GitSidebar.tsx` |
| Remove redundant status subprocesses only where existing porcelain output provides the same authority; retain authoritative binary handling in diff | Merged | `git/status.rs`, `git/command.rs`, `git/diff.rs` |
| Cache repository identity and latest authoritative status/diff metadata by connection/root/generation; coalesce identical in-flight requests; cancel/ignore on scope change | Merged | `git/coordinator.rs` (`RepositoryKey`, `RepositoryCapabilities`), `repositoryStore.ts` |
| Keep remote mutations pessimistic for authority but make the pending target visible immediately; on acknowledgement consume the resulting authoritative refresh | Merged | `git/mutation.rs`, `repositoryStore.ts` (`mutate`), `GitSidebar.tsx` |
| One repository coordinator keyed by client/server/epoch/root token/repository identity, owning bootstrap promise, native watcher, subscribers, in-flight status, cached static capabilities and mutation reconciliation; capped-backoff native-watch fallback with no permanent 750 ms full pipeline | Merged | `git/coordinator.rs`, `coordinator/watcher.rs`, `coordinator/subscribers.rs` |
| Batch the three static `rev-parse` queries on a cache miss and derive branch/HEAD from authoritative porcelain status; preserve the visible binary badge until one-process equivalence is proven | Merged | `git/command.rs`, `git/status.rs` |
| A matching diff opens in one RTT from cached/watch status, its response carrying validated authoritative status; remove the prerequisite status RTT, duplicate diff watch and unused patch body; route large old/new bodies over the persistent bulk lane | Merged | `GitDiff.old_content_ref`/`new_content_ref`, `GitDiffContentRef/Request/Chunk`, `Operation::GitDiffContent = 45`, `git/content.rs`, `connection/git_content.rs`, `useSharedGitDiff.ts` |
| Consume mutation-returned authoritative status; stage/unstage/commit is one request and one post-command status pipeline; diff actions request only a remaining diff; scope-check completions and retain cancel-before-registration tombstones | Merged. The tombstone half is now served by P3's `OperationRegistry` rather than P4's deleted `GitOperations` — see conflict decision 2. | `requests/git_dispatch.rs`, `git/mutation.rs`, `connection/operations.rs`, `repositoryStore.ts` |

### P4 deferrals and unmeasured rows

- **The plan's Git *outcome* row — "warm panel is zero requests; a matching diff
  is one RTT; stage/unstage/commit is one request and one post-command
  pipeline; no unchanged 750 ms fallback polling" — is proven by deterministic
  host fixtures, not by a shaped-SSH remote lane.** The two fixtures are
  `phase14_thirty_two_consumers_report_native_watchers_and_status_processes` and
  `phase14_warm_diff_and_mutation_process_counts`, both opt-in `--ignored`
  tests. No shaped 100 ms Git journey was captured in this integration.
  Reported as fixture-proven, never as remote round-trip evidence.
- **No `before` Git evidence exists**, for the same structural reason as P3's:
  the Wave 1 ledger records the mandatory Stage 0 shaped baseline as
  `NOT_CAPTURED` and it cannot be reconstructed after the fact. The Git numbers
  are absolute, not comparative.
- **`git/path.rs::lossless_stat_component` still panics** on an oversized stat
  component. P3 fixed its filesystem twin and flagged the Git copy as P4's;
  P4 did not fix it. Carried forward as a known defect in P4-owned code.
- **The packaged-app / `cua` QA stage is not part of this integration.** Linux
  deterministic lanes cannot claim macOS/WKWebView rows.

## Residual findings, carried forward unfixed

Raised by the integration review, verified against the code, and deliberately
not acted on in this integration. None is presented as absent.

### R1. The file lane and the Git lane carry bulk bodies two different ways

P3 added `OpenFileStream` as a **push stream**: one request, one
`FileStreamFrame` header on `Envelope.payload = 20`, continuous body frames,
one terminal response. Its stated justification — in the proto, in
`open_stream.rs`, and in the plan's required outcomes — is that the previous
`2 + ceil(bytes/1 MiB)` staircase was unacceptable on the remote link.

P4 added `GitDiffContent` on the **same bulk lane** as a **pull staircase**:
`GitDiffContentRequest { offset, length }` → `GitDiffContentChunk` inside
`GitResponse`, one full request/response per chunk. The arithmetic, from the
merged constants:

- `INLINE_DIFF_BODY_LIMIT = 256 KiB` combined — below this nothing is deferred.
- `MAX_DIFF_CONTENT = 10 MiB` per side.
- `GIT_CONTENT_CHUNK = 1 MiB`, and `stream_sides` iterates sides and chunks
  strictly sequentially.

So a deferred diff with two large sides costs up to **20 sequential round
trips**, about two seconds of pure RTT on the plan's mandatory shaped 100 ms
fixture.

This is a shortfall against the round's spirit rather than a missed acceptance
criterion, and the ledger says which: the plan requires the *diff* to open in
one RTT (met — the control response carries its validated authoritative status)
and large bodies to go over the bulk lane rather than head-of-line blocking
terminal traffic (met). It does not literally require the deferred body itself
to be one RTT. The "10 MiB does not form a per-MiB RTT staircase" outcome is
written about the file open, which does meet it.

The pull shape also drags in machinery the push shape does not need:
`CachedDiffBody` and `DiffBodyKey` in `git/content.rs` exist only because the
read is chunked and a later chunk must find the same bytes; `stream_sides`
re-implements offset ordering, size agreement, last-chunk and stall detection
that `FileReadStream` already does; and `Exchange::on_frame` is typed
`FnMut(v1::FileStreamFrame)` — a file-lane type on a struct widened to
`pub(crate)` for the Git lane, which then has to explain why it passes `None`.

The convergent design is one lane-neutral body frame, `GitDiffContent` emitting
header + frames + terminal response the way `file_stream_dispatch` does, and
one receiving state machine. That deletes `GitDiffContentRequest.offset/length`,
`GitDiffContentChunk`, `CachedDiffBody`, `DiffBodyKey`, `stream_sides`'
validation loop and the `on_frame: None` special case, and turns 20 RTTs into
one. It is a protocol, host and desktop change and belongs to a package owner,
not to a merge.

### R2. `GitContentReads` is a third per-connection job registry

`connection/git_content.rs` provides, for the Git diff-body lane, what
`files/scheduler.rs` already provides for every other bulk read: a bounded map
of cancellable jobs, an RAII removal guard, `cancel_all()` on connection
replacement, and an admission bound. This merge already deleted one duplicate
registry (`GitOperations`, conflict decision 2) on exactly that reasoning, and
did not apply it here.

It is not merely deleted in favour of the scheduler, because the scheduler's
two-active bound is process-global and shared with multi-gigabyte transfers: a
diff body queued behind a 5 GiB download is worse than the duplication. The
convergent move is to extract the shared "per-connection cancellable job"
primitive so there is one answer rather than three — which is easiest after R1,
since the diff-body read then has the same shape as `run_file_read`.

Related, and smaller: `git_content.rs` defines its own renderer channel
dialect (`FRAME_CHUNK = 1`, `FRAME_COMPLETE = 2`, `FRAME_ERROR = 3`) while the
file lane on the identical `Channel<InvokeResponseBody>` uses kind `2` for a
chunk and `1/3/4` for JSON states. The encodings were verified to match their
own decoders byte for byte; the objection is that there are two.

### R3. `useSharedGitDiff` publishes three raw mutable refs

`DiffPaint` hands the rendering surface `pending`, `lifecycle` and `committed`
as `MutableRefObject`s, and `lifecycle` **is** the hook's own request
supersession serial. `GitDiffSurface` then writes
`paint.committed.current = paint.lifecycle.current`, reconciling the hook's
generation counters on its behalf — while the hook's docstring says the surface
"owns none of it". The effect predicate and the Monaco `onMount` predicate also
disagree about whether a committed generation must match.

Not fixed because the machinery is measurement-only and `INERT_PAINT_TICKET`
makes it a no-op when the probe is disabled, so the leak has no product
consequence today. It is real, and **P6 should fix it while moving this
boundary**: the surface should report facts (`notePainted(surface)`) and the
hook should own the decision.

### R4. `spawn_watcher` is one long function with many interacting flags

`filesystem/watch_service.rs` runs `native_dirty`, `native_rescan`,
`native_failed`, `owed_rescan`, `failed_rescans`, `rescan_failed`, `all_rescan`
and `woken` through two nested `select!`s in one body. The asymmetry that hid
the backoff hole fixed in `f024009` was undetectable at this size. The
convergent shape is a `NativeSignals { dirty, rescan, failed }` with
`take_batch()`/`owe_rescan()` and a `RescanBackoff` with `record`/`delay`, after
which the loop reads wait → take → route → collect and there is one place that
can re-arm. This is P3-owned structure, listed in its own round-6 residue as
S1–S10, and was not attempted here.

### R5. Coordinator cache state mutated from outside the coordinator

`git/coordinator.rs` exposes `pub(super) diff_body: Mutex<Option<CachedDiffBody>>`
and `git/content.rs` implements lookup, insert and release-by-key against the
raw field — cache policy for repository state living outside the type that owns
the repository. Disappears under R1.

### R7. One shared-observation concept, three implementations

"A scope-keyed, refcounted, cancellable shared remote observation with a
bootstrap promise, an LRU of remembered state, and stale-scope guards" exists
three times in this integration, written by two authors in three architectures:

| | Where | Shape |
| --- | --- | --- |
| Git repositories | `features/git/repositoryStore.ts` | Plain classes outside React; `useWorkspaceGit` is 78 lines over `useSyncExternalStore` |
| Explorer directories | `features/files/useWorkspaceFiles.ts` (744 lines, twelve refs) | Everything inside React, with `stateRef`/`scopeRef` mirrors and a hand-rolled `scopeEpoch`; helpers take `root` as a parameter precisely because the ambient ref can move under a deferred caller |
| Directory watches | `features/files/api.ts` (`WatchRecord`) | A third refcount/bootstrap/retire implementation |

The LRU eviction idiom is written twice, structurally identical, in
`directoryCache.ts` and `repositoryStore.ts`. Within the Git lane alone the same
store is consumed two ways: `useWorkspaceGit` through `useSyncExternalStore`,
`useSharedGitDiff` through `useEffect` + `acquire` + five mirrored `useState`
slots, one of which (`sharedError`) exists only to un-mirror the mirror.

The Git lane's answer is the better one and it is one directory away. Two
follow-ups, in order of cost:

1. **Small, owned, and should land before P6:** one `useGitRepository(store,
   scope, root)` used by both Git consumers, deleting `sharedError`, the
   `repository` state, and most of `loadWhenStatusMoves`.
2. **Large, and a package owner's decision:** move the Explorer's scope-owned
   mutable state into an `ExplorerObservation` the hook subscribes to, which
   takes `stateRef`, `scopeRef`, `scopeEpoch`, the ref relays in R6 and the
   "root as a parameter because the ambient ref moved" workaround with it — or
   an explicit written decision that the two subsystems keep two architectures,
   and why.

### R8. Two hand-written bulk-`Channel` readers in the renderer

`files/api.ts`'s `#fileIo` and `git/api.ts`'s `readDeferredBodies` are two
~85-line readers with the same structure — tag-byte dispatch, big-endian u64
offset, sequence validation, a `settled` flag, an abort listener, and the
"the transfer id can land after we gave up" handling — written independently by
the two lanes with different frame dialects. This is the renderer half of R1 and
R2 and belongs in the same follow-up.

### R9. Watcher recovery scheduling and coordinator capability lifetime

Both raised by the round-2 Rust reviewer, both verified, both in P4-owned
recovery code that no remaining review round could check:

- `coordinator/watcher.rs` uses one `fallback` variable for two unrelated
  schedules. `refresh` doubles it on the **native** signal path whenever
  `source_generation` is unchanged, so a healthy watcher receiving change
  signals that produce identical status walks the *re-establishment* backoff
  toward `FALLBACK_MAX = 30 s`, and nothing resets it when `establish`
  succeeds. A watcher that later breaks can therefore wait up to 30 s for its
  first retry, having earned that delay entirely while healthy. Split
  `poll_interval` from `establish_backoff` and reset the latter on success.
- `coordinator.rs`'s `capabilities()` clears the slot on revalidation failure
  and rediscovers, producing a new `Arc`, but a running `observe` task captured
  the old one by value and `ensure_watcher` returns early while a watcher is
  installed. Reachable when the root inode survives while the metadata
  capability does not — `rm -rf .git && git init` keeps `validate_token`
  passing while `revalidate` fails. Call `stop_watcher()` where the slot is
  cleared.

### R6. Smaller, verified, not acted on

- `envelope.proto` is now 1,032 lines and holds the terminal, filesystem, Git,
  agent, transfer and test surfaces in one file.
- `CAP_FILE_STREAM` was added with an explicit rule — a refusal must name the
  missing capability. `GitDiffContent`, an equally new required bulk operation,
  got no bit and gates on the per-connection `bulk_available` flag instead.
- `listing_page.rs::token_key()` builds a 32-byte keyed-hash key by repeating a
  16-byte UUIDv4, so it carries 122 bits duplicated rather than 256.
- `active_root_dispatch.rs`'s `known_root_token` fast path skips the
  cancellation check the slow path performs.
- `file_stream.rs` bounds a streamed open only by the host-declared
  `total_bytes`; there is no independent ceiling at the Rust trust boundary.
- `languageForPath` is duplicated verbatim between `AppTabSurface.tsx` and
  `GitDiffSurface.tsx`, as are the Git result formatters in `GitSidebar.tsx`
  and `GitDiffSurface.tsx`.
- The Monaco paint-surface harness (four refs plus `bindEditorHost` plus the
  `detachLayout` dance) is copy-pasted between the two editor surfaces. This
  **predates both branches**, so it is not a regression — but P6 owns the lazy
  boundary in exactly these two files, and extracting `useEditorPaintSurface()`
  first is what makes P6 a small change rather than a two-file rewrite.
- `useWorkspaceGit` consumes `GitRepositoryStore` through `useSyncExternalStore`
  while `useSharedGitDiff` consumes the same store through `useEffect` +
  `acquire` + five mirrored `useState` slots. One `useGitRepository` hook would
  delete `sharedError`, the `repository` state and most of
  `loadWhenStatusMoves`.
- `rearmRoot` / `noteRootActivity` in `useWorkspaceFiles.ts` are refs wrapping
  callbacks `useActiveRoot` already returns with stable identities. They exist
  only because `useActiveRoot` is called below three of its callers; removing
  them means hoisting that call, which is a dependency-ordering change with no
  behavioural payoff. (`rootProbeSerial`, which was simply dead, is deleted.)
- `AppTabSurface.tsx:273` uses `paint.surfaceGeneration !== 0` to mean "this
  surface will mount Monaco", riding on a mutable counter's unset value.
- Three `eslint-disable-next-line react-hooks/exhaustive-deps` comments exist
  and **there is no ESLint in the repository** — no config, no lint script.
  Either wire it up or drop the suppressions; today they assert a rule that is
  not being checked.
- `listing_page.rs` documents a cached page as answered "out of memory without
  touching the filesystem or anything else at all". It is not: two
  `RootCapability::capture` calls precede the cache, each a `canonicalize` +
  `openat` + `fstat`, and `snapshot::server_identity()` forks
  `tmux display-message` on every call. The third fork `PageBinding` had added
  was removed on the branch, which is real, but the fast path is still two forks
  and two directory opens from free. Memoize `server_identity()` per connection
  and hand the already-validated capability to the worker.
- `DirectorySnapshot.overflowed` is now redundant: `snapshot()` sets it to
  `next_page_token.is_some()` and `complete` to its negation, and the renderer
  has stopped reading it. Two wire fields with `overflow` in the name and
  unrelated meanings is a surface that gets misread once and stays misread;
  retiring `overflowed` would make `recovered_from_overflow` unambiguous.
- `filesystem.rs` was not decomposed the way its diffstat suggests: production
  content grew 446 → 512 lines while its test module moved out. All eight
  submodules open `use super::*`, so it is a glob prelude with no module
  boundary. The extractions that did happen — `open_stream.rs`,
  `listing_page.rs`, `watch_fallback.rs`, `failure.rs`, the five test modules —
  are real; the shared vocabulary should be a `common.rs` the submodules name.
- `HOST_CAPABILITIES` and `CAPABILITY_NAMES` are two hand-maintained lists of
  the same sixteen constants. Drift is caught by a `count_ones` assertion and
  the third copy in `diagnostics.rs` was deleted on the branch; deriving one
  from the other would retire both the second list and its guard.
- `coordinator/subscribers.rs` has the same nine-line body twice, differing in
  one `GitEvent` field.
- Everything in P3's own "Round 6 residue" list above, unchanged.

## Integration-level thermo-nuclear review

Every reviewer was a fresh subagent on opus, instructed to read the complete
exact `/home/operator/dev/worktree-cli/skills/thermo-nuclear-code-quality-review/SKILL.md`
before looking at any code, and given the plan's P3/P4/Guardrails/Deferrals
sections plus P3's branch ledger as specification. No reviewer was told what
this agent had decided, what any earlier round found, or that anything was
suspected anywhere. The three areas each round was pointed at — the
merge-conflict resolutions, the bulk-lane sharing, and the protocol/policy
surface — were stated as scope, not as suspicion.

### Round 1

| Reviewer | Lane | Verdict | Blockers |
| --- | --- | --- | --- |
| `wave2_rust_review_1` (task `a3f04dbd766e0f814`) | Rust / host / bridge / protocol | **BLOCK** | B1 two incompatible bulk-body protocols on one lane; B2 `GitContentReads` as a third per-connection job registry; B3 the editor save path discarding `_expected_generation`. Plus M1 `spawn_watcher` size and an asymmetric rescan backoff, M2 coordinator state mutated from outside, M3 the `git/path.rs` panic. |
| `wave2_ts_review_1` (task `ad428b61ebe3b4895`) | TypeScript / React | **BLOCK** | B1 two hand-rolled abort races beside a canonical `abortable` that predates both branches; B2 `useSharedGitDiff` publishing three raw `MutableRefObject`s; B3 three "latest ref" idioms, one written during render; B4 the recovery queue drained non-atomically. |

Both rounds independently reached the same diagnosis: **the two packages did not
converge.** Each re-solved problems the other had already solved, and the merge
preserved both answers.

Fixed in `f024009`, each verified against the code before acting:

| Finding | Disposition |
| --- | --- |
| TS-B4 recovery drain | **Fixed.** The all-or-nothing clear on map identity meant one event arriving between the render that produced the batch and the update that cleared it made the whole queue look undrained; the effect re-ran and re-issued every entry. `coalesceRecovery` is idempotent and survives that, `restorePages` is not — the second call aborts the first and the aborted one's teardown clears the directory's wait state while its replacement is still running. The rule moved to `directoryState::consumeRecoveries`, beside the `oweRecovery` precedence it has to preserve, with four deterministic tests including the superseded-while-in-flight case. |
| Rust-M1 rescan backoff | **Fixed.** Three sites re-arm `native_rescan` and only one fed `failed_rescans`, so a directory failing either precise-event path durably drove an uncounted rescan turn, and a turn whose own sweep happened to publish reset the counter. That is the un-backed-off ten-per-second re-list the counter exists to stop, reached without entering the branch that fed it. All three now count; the clear still requires a completed sweep. |
| Rust-B3 discarded save generation | **Fixed**, and not the way the finding assumed. Enforcement compares the **leaf** generation — what listings, precise watch events and the open all report — not the resolved write target's. For a symlink those are different inodes, and comparing against the target would have refused every save of a symlinked file; the existing symlink test caught exactly that and is unchanged. Zero still means "no claim". New regression covers stale-refused, fresh-accepted, and unclaimed-unaffected. |
| TS-B1 duplicated `abortable` | **Partly fixed, and this ledger overstated it.** `abortable`/`throwIfAborted` moved to `src/transport/abortable.ts` and the Explorer lane's two hand-written copies went. An earlier revision of this row claimed *"every `new DOMException(..., 'AbortError')` in both features now goes through one `cancelled()`"* and the commit message claimed all three sites called the helper. **Both were false**: `SharedDiffRequest.join` in `repositoryStore.ts` kept a fourth hand-rolled race and only its rejection line was changed. Round 2 caught it; it is collapsed onto `abortable` in `9c7acf4`. The claim is corrected here rather than deleted, because the artefact that records convergence overstating it is the failure worth recording. |
| TS-B3 three latest-ref idioms | **Fixed.** One `src/commands/useCommittedRef.ts`, committed in a layout effect, at three sites. `GitSidebar`'s render-time write — which `ExplorerTree`'s own comment names as wrong for this exact publication — is gone. `ExplorerTree`'s forwarding-object ref keeps its own early declaration, because its `useMemo` reads it during the same render. |
| Rust-M3 `git/path.rs` panic | **Fixed.** `from_stat` is fallible and propagates; the widening lives in one generic checked boundary rather than target-specific casts. Kept generic deliberately — on Linux the conversions are identity and `clippy::useless_conversion` rejects the concrete form, while on macOS `st_dev` is `i32` and `st_mode` is `u16` and they are real. |
| Rust-B2 `MAX_IDLE` arithmetic | **Comment corrected, bound unchanged.** The reviewer was right that the stated arithmetic does not hold: the transfer engine's two-active bound is process-global while the Git diff-body bound is per connection, so N profiles give 2 + N concurrent bulk consumers. **An earlier revision of this ledger asserted that arithmetic held; that assertion was wrong and is retracted.** `MAX_IDLE` is a retention budget, not a concurrency bound — exceeding it costs a re-dial, never correctness — and now says so. |
| Reviewer preference: operation-policy list completeness | **Taken.** The hand-written 49-entry compatibility list had nothing forcing a new operation into it. A guard now fails when the enum grows past it. The list was also audited programmatically at merge time: 49 operations in the schema, 49 `assert_policy` calls, none missing and none extra. |

Argued against, with the reason, rather than quietly dropped:

- **Rust-B1, unify the two bulk-body protocols.** The diagnosis is correct and
  is recorded below as this integration's largest residual. The remedy is not:
  renaming `FileStreamFrame` to a lane-neutral body frame, reshaping
  `GitDiffContent` into a push stream, and hoisting the receiving state machine
  is a protocol, host and desktop redesign of a package whose branch gate has
  already closed. A merge agent's mandate is to integrate and gate, not to
  redesign a package under it and land the result on one further review round.
  It is written up in full, with its arithmetic, so it cannot be lost.
- **Rust-B2, route Git diff bodies through the transfer engine.** Rejected on
  the merits. That engine's two-active bound is process-global and shared with
  5 GiB file transfers; putting a diff body behind it would head-of-line block
  the diff surface behind an unrelated download — worse than the duplication it
  removes. The duplication itself is recorded as residual.
- **TS-B2, `DiffPaint`'s three raw refs.** A real boundary leak: the surface
  owns and mutates the hook's supersession serial. But the machinery is
  measurement-only and inert when the probe is disabled, and rewriting P4's
  paint-generation matching at integration time is worse odds than the leak.
  Recorded as residual, and it is P6 handoff material because P6 moves exactly
  that boundary.
- **Rust-M2, coordinator `diff_body` mutated from `git/content.rs`.** Correct
  encapsulation point; it disappears under B1 and is recorded with it.

### Round 2

Fresh reviewers again, same neutral framing, no knowledge of round 1 or of
anything it changed.

| Reviewer | Lane | Verdict | Blockers |
| --- | --- | --- | --- |
| `wave2_ts_review_2` (task `a0adb795dbc51adb7`) | TypeScript / React | **BLOCK** | B1 `SharedDiffRequest.join` still a fourth hand-rolled `abortable`, and the round-1 commit claiming otherwise; B2 four render-time ref writes contradicting the helper round 1 added to forbid them; B3 a refused watch's fallback list aborting itself on every unrelated tree interaction; B4 `DirectoryRequests`' doc stating a rule it enforces for one of the two kinds it names; B5 the `DiffPaint` boundary; B6 dead `rootProbeSerial` and two needless ref relays; B7 an orphaned doc comment. Plus S1, the structural non-convergence finding. |
| `wave2_rust_review_2` (task `ae6e3eee87fafa302`) | Rust / host / bridge / protocol | **BLOCK** | B1 two mechanisms for a large body on one bulk lane, with a concrete retention claim that does not hold; B2 `bulk_available` guessing a client fact from `!read_only`; B3 unbounded client-keyed Git subscribers, which also makes `MAX_TRACKED_REPOSITORIES` not a bound. Plus ten preferences, several of them concrete. |

**The Rust round-2 reviewer independently confirmed all three merge conflict
resolutions as correct**, checked against the code rather than the commit
message — including that `Exchange::on_frame` remaining `pub(super)` is
load-bearing, because it means the sibling Git module physically cannot
construct an exchange that accepts a file body frame, which is what lets
`bulk_protocol` treat one as a transport error rather than something silently
accepted. That is the integration's central deliverable, verified by a reviewer
that was told nothing about it.

Fixed in `9418150`:

| Finding | Disposition |
| --- | --- |
| Rust-B1 diff-body retention claim | **Comment corrected; the mechanism is left.** The slot is released only when the last chunk is served, so an abandoned read never reaches it and a subscribed coordinator is never evicted. The true bound is one body per tracked repository — `MAX_TRACKED_REPOSITORIES` × `MAX_DIFF_CONTENT`, finite and therefore not a leak, but far weaker than the "one in-progress body" the comment asserted. Removing the cache outright depends on the protocol convergence in R1. |
| Rust-B2 `bulk_available` | **Fixed.** Now `!read_only && requested_capabilities & CAP_BULK_DOWNLOAD != 0`. The field's doc calls it a capability fact and `!read_only` was a proxy that happens to coincide for this desktop; any other non-read-only control client received a body reference it could not fetch, and an empty diff with it. |
| Rust-B3 unbounded Git subscribers | **Fixed.** `MAX_SUBSCRIBERS = 128`, mirroring `MAX_WATCHES` on the filesystem side of the same round, and four times the 32-consumer figure the plan is written around. The key is client-supplied, and because a subscribed coordinator is never evicted the uncapped map was also what made `MAX_TRACKED_REPOSITORIES` not a bound in the case its own comment describes. |
| Rust-pref 2 debounce no longer coalesced | **Fixed, and it is a regression against the base.** `0faf78c`'s `watch.rs` drained the signal channel after resting; the rewrite lost it. The channel holds one permit, so an event inside the settle window fired the instant the refresh returned and every burst cost two pipelines — the opposite of what `WATCH_DEBOUNCE` documents. **Measured**: the phase-14 mutation fixture went from 3 status pipelines / 3 status processes / 12 Git processes to 2 / 2 / 9. |
| Rust-pref 8 the policy guard did not check its own name | **Fixed, and it was mine.** The guard added in `f024009` asserted a hard-coded count of 49 — satisfiable by bumping the number while the hand-written list silently stops covering the enum, which is exactly the failure it was written against. The list is now one table read by both tests, and the coverage test names any operation with a policy and no assertion. |
| Rust-pref 7a inlined `require_authoritative` | **Fixed.** `validate_status_generation` calls it rather than repeating its body four lines above the definition. |
| Rust-pref 9 false every-exit invariant | **Fixed.** `file_stream_dispatch`'s header no longer asserts that every exit responds; the three frame-send failures return without responding precisely because the sequencer channel has closed and nobody is waiting. |

Argued against or recorded rather than fixed:

- **Rust-B1's remedy**, as with round 1: the protocol convergence is R1 below.
- **Rust-pref 1, the watcher's single `fallback` variable serving both the poll
  interval and the re-establishment backoff.** Concrete and correct — a healthy
  watcher receiving signals that produce identical status doubles the
  *re-establishment* backoff toward 30 s, and nothing resets it on a successful
  establish, so a watcher that later breaks can wait up to 30 s for its first
  retry having earned that entirely on the healthy path. Splitting the two
  schedules is the right fix and is real work in P4-owned recovery code with no
  review round left to check it. **Recorded as R9.**
- **Rust-pref 3, capabilities replaced under a live `observe` task.** Reachable
  (`rm -rf .git && git init` keeps the root token valid while revalidation
  fails); the task captured the old `Arc` by value. **Recorded as R9.**
- **Rust-prefs 4, 5, 6, 7b, 7c, 10** — the `listing_page` fast path still paying
  two `RootCapability::capture`s and a `tmux display-message` fork; retiring the
  now-redundant `overflowed` wire field; `filesystem.rs` being a glob prelude
  rather than a decomposed module; deriving `HOST_CAPABILITIES` from
  `CAPABILITY_NAMES`; the duplicated nine-line subscriber bodies; and
  `envelope.proto` crossing 1,000 lines. **Recorded in R6/R9.** The proto split
  is additionally covered by the plan's explicit "no protobuf request hierarchy
  rewrite" deferral.

### Review conclusion

Two rounds, four independent reviewers, cap reached. Round 2 returned BLOCK in
both lanes, and its blockers were substantively different from round 1's — the
signal had not degraded to repeats, so the cap is genuinely binding here rather
than a natural stopping point. **Residual findings R1 through R9 below are real
and unfixed**, and the two most valuable are named with owners: R1 (converge the
two bulk-body protocols) belongs to whoever owns the Git package next, and R7's
small half (one `useGitRepository`) should land before P6.

The through-line of all four reviews was the same and is the honest summary of
this integration: **the merge is correct, and the two packages did not
converge.** Every conflict was resolved semantically and every gate is green,
but P3 and P4 independently built two answers to bulk bodies, two answers to
per-connection job registries, two answers to shared scoped observation, and two
answers to renderer bulk channel decoding. Four were reconciled during this
integration; the rest are written down rather than absorbed.

Round 2's TS lane made the same diagnosis as round 1 and pushed it one level
harder: **the remediation had claimed convergence the code did not have.** That
is the most serious class of finding here, so it is recorded prominently rather
than folded in.

Fixed in `9c7acf4`:

| Finding | Disposition |
| --- | --- |
| TS-B1 the fourth `abortable` | **Fixed**, and the false claim corrected above. `join` now calls `abortable(this.promise, signal, depart, …).finally(depart)`; `depart` was already idempotent, so the abort path and the settle path cannot double-count. |
| TS-B2 four render-time ref writes | **Fixed.** `stateRef`, `scopeRef`, `useActiveRoot`'s `latest` and `useSharedGitDiff`'s `currentLoad` all use `useCommittedRef`. Every read site was audited first: all are inside callbacks and effects, none in a render body, so the layout-effect commit is behaviour-preserving and strictly safer. |
| TS-B3 fallback-list storm | **Fixed**, and it is a real correctness bug on the primary remote path. `sync()` runs on every expand and collapse anywhere in the tree; `onDeferred` guarded only on "has the tree got a listing", which an in-flight read has not installed, so each interaction re-issued the read and `open(directory, "list")` aborted the one already fetching. The question now goes to `DirectoryRequests.reading(path, kind)` — the owner of the fact — and that class got the test file it never had, six tests. |
| TS-B4 doc vs code on supersession | **Doc corrected to match the code.** `"page"` extends rather than replaces and is nobody's rival; `"list"` and `"restore"` both answer "what does this directory contain now" and the later question wins, in both directions. Pinned by tests either way round. The code was not changed: mutual supersession is the right behaviour, the general claim in the comment was not. |
| TS-B5 `DiffPaint` raw refs | **Fixed**, reversing round 1's deferral. Two independent reviewers reaching it is signal. `DiffPaint` is now `noteCommitted()` + `notePaintable(editor?, onPaint?)`; the surface supplies `EditorSurfaceFacts` with live readers, because a surface can remount between a measurement being armed and its pixels landing. The hook keeps the decision. |
| TS-B6 dead `rootProbeSerial` | **Deleted.** The two ref relays (`rearmRoot`, `noteRootActivity`) are left: removing them means hoisting the `useActiveRoot` call above three of its callers, which is a dependency-ordering change with no behavioural payoff, at the end of the last review round. Recorded in R6. |
| TS-B7 orphaned doc comment | **Fixed.** The comment describing `INERT_ROW_ACTIONS` was sitting above `ROW_METRIC_CLASS`. |

Argued against:

- **TS-S1, one concept implemented three times** (Git repositories as a plain
  store outside React, Explorer directories entirely inside it, directory
  watches a third way). The diagnosis is correct and the comparison is fair —
  `useWorkspaceGit` is 78 lines over `useSyncExternalStore` where
  `useWorkspaceFiles` is 744 with twelve refs for the same job. The requested
  "small half", one `useGitRepository` both Git consumers share, touches the
  same file as B5; doing both in one unreviewed pass compounds risk at the point
  where no further round remains. **Recorded as R7 below and named as owned
  follow-up work**, which is what the reviewer asked for if it was not done.
- **The Explorer half of S1** — moving scope-owned mutable state into an
  `ExplorerObservation` the hook subscribes to — is a rewrite of P3's core, not
  an integration fix.

## Gate results

All run from `/home/operator/dev/muxflow` on the merged head, with
`CARGO_TARGET_DIR=tmp/target` and
`TAURI_CONFIG='{"bundle":{"externalBin":[]}}'` where the Tauri context needs
it. `TAURI_CONFIG` is a build-time shim for the test gate only — it overrides
`bundle.externalBin`, which `tauri::generate_context!` resolves at compile time
against a sidecar binary this checkout does not build. It changes no shipped
configuration.

| Gate | Result |
| --- | --- |
| `pnpm --filter @tmux-agent-ide/desktop check` | Pass. |
| `pnpm --filter @tmux-agent-ide/desktop test` | Pass: **93 files, 835 tests**. At the merge itself it was 825 — P3 alone was 91/804 and P4 alone reported 736 against the same base, so the sum is consistent and neither branch's suite was lost. The two review rounds added ten, in `directoryState.test.ts` and the new `directoryRequests.test.ts`. |
| `pnpm build` | Pass. Only the pre-existing Vite chunk-size advisory. `monaco` chunk 3,980.17 kB, `index` 1,051.48 kB, `AppTabSurface` 85.38 kB, `GitDiffSurface` 10.42 kB. |
| `cargo fmt --all -- --check` | Pass. |
| `git diff --check` over `0faf78c..HEAD` | Pass. It first flagged a new blank line at EOF in `ExplorerTree.tsx` (P3) and `GitSidebar.tsx` (P4); both were removed in `bb9301b`. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass, with no command-line allowances. **It failed first** on `private_interfaces` for `claim_file_operation`; that is the merge's own breakage and is fixed in conflict decision 4, not waived. |
| `cargo test --workspace --all-targets -- --test-threads=1` | Pass, **EXIT=0, 0 failed**, rerun on the final head. tmux-agent-desktop 206 passed / 3 opt-in ignored; tmux-ide-host 358 passed / 2 opt-in ignored; protocol 3 unit + 21 round-trip; tmux-control 33 / 3 opt-in ignored; hook CLI 4; local tmux integration 2; phase0-core 5. |
| `bash tests/performance/optimization/run-explorer-after.sh integration-wave2` | Pass, exit 0. Artifacts `tmp/phase14/explorer-integration-wave2/`. |
| phase-14 `git-consumers` fixture | Pass. |
| phase-14 `git-processes` fixture | Pass, and it **improved** during review: restoring the lost watch-debounce drain took the mutation lane from 3 status pipelines / 3 status processes / 12 Git processes to **2 / 2 / 9**. |
| phase-4 protocol driver, local lane | Pass, exit 0; the full `jq` gate matched. |

### Phase-14 Explorer lane, exact numbers

```
explorerListWatch            directoryListRequests 1, watchSubscribers 2, watchRequests 1,
                             watchReleases 2, activeWatchHighWater 1,
                             hostRequestAttempts 3, successes 3, failures 0, cancellations 0
explorerWide                 entries 4096, logicalRowsHighWater 4096,
                             renderedRowsHighWater 46, rowParity true,
                             mountedRowsAtFourTimesTheEntries 46
explorerWideWatchTraffic     rootWatchRequests 1, expandWatchRequests 1,
                             directoryListRequests 0, collapseWatchReleases 1,
                             expandedRows 4096, externalChangeRows 4097,
                             cachedRevisitPaintedBeforeRevalidation true,
                             jsdomExpandToPaintP95Ms 32.626,
                             jsdomExternalChangeToPaintP95Ms 29.805
explorerPaginatedWatchTraffic entries 12288, hostPageSize 4096, heldListingComplete false,
                             expandDirectoryListRequests 1, expandedRows 8192,
                             changeDirectoryListRequests 0, externalChangeRows 8192
```

The two `jsdom*P95Ms` figures are **jsdom durations, not browser paint**, and
are not the plan's 150 ms / 300 ms budgets. They are recorded because they are
what this lane can honestly produce; those budgets remain uncollected (P3
deferral 1). `mountedRowsAtFourTimesTheEntries 46` at both 4,096 and 16,384
entries is the windowing invariant, and it is a function of
`explorerWindow.ts`'s 480 px fallback viewport because jsdom reports
`clientHeight === 0` — the invariant is the constancy, not the number.

### Phase-14 Git fixtures, exact numbers

```
git32Consumers        consumers 32, subscriberHighWater 32,
                      nativeWatchers 1, nativeWatcherCreations 1,
                      watchRegistrations 1, discoveries 1,
                      statusPipelines 1, statusProcesses 1,
                      diffProcesses 2, gitProcesses 4, activeProcessHighWater 1
gitProcessAccounting  warmDiffStatusPipelines 0, warmDiffStatusProcesses 0,
                      warmDiffGitProcesses 4,
                      mutationStatusPipelines 2, mutationStatusProcesses 2,
                      mutationGitProcesses 9,
                      nativeWatcherCreations 1, totalDiscoveries 1
```

32 consumers of one repository produce exactly one native watcher and one
coalesced status pipeline, which is the plan's stated Git outcome. A warm diff
runs **zero** status pipelines. These are deterministic host-process counts;
they are not shaped-SSH round-trip evidence and are not presented as such.

### Phase-4 protocol driver, local lane

```json
{"activeRootAtomic":true,"blake3Verified":true,"controlBodiesRejected":true,
 "dotfilesVisible":true,"editorMaxReadWriteBytes":10485760,"folderDownload":true,
 "gitCollapsed":true,"maxControlLatencyMs":5,"nonEmptyConfirmation":true,
 "oversizedPreviewMetadataOnly":true,"previewBoundaryBytes":26214400,
 "rootToken":true,"streamedDownloadBytes":21,"textRoundTrip":true}
```

This lane is **outside the cargo workspace** (`Cargo.toml` excludes
`tests/integration/filesystem/protocol-driver`), so `cargo test --workspace --all-targets`
cannot see it and no other gate above runs it. It is the only gate that
exercises `Operation::ReadFile`'s replacement end to end. The full
`tests/integration/filesystem/run-backend.sh` additionally needs Docker and a Debian-compatible
helper build for its remote half, which was **not** run here.

### Gates deliberately not run in this integration

The plan's "Integrated automated envelope" also lists the Phase 12 performance,
idle-steady, pause-probe, VT-parity and stall lanes, the Phase 7 smoke, and the
shaped 100 ms Docker journeys. **None was run for Wave 2.** Wave 1 captured
them against `18461d1`/`4d2120f`, and neither P3 nor P4 touches the terminal
delivery, transfer, or tmux paths those lanes measure. They are recorded here
as not-run rather than inherited as passing, and the shaped remote Explorer /
file-open / Git journeys the plan asks for remain uncaptured for Wave 2.

## Handoff to P6

P6 runs next directly on this branch. What it needs to know:

### The Monaco / file-I/O ordering handoff — the one required outcome P3 could not meet

P3's plan bullet "Start remote file I/O before Monaco/editor evaluation" is
**UNMET**, and it is unmet for a reason that only P6 can fix:

- `apps/desktop/src/app/App.tsx:79-80` lazy-loads **both**
  `features/shell/AppTabSurface` and `features/git/GitDiffSurface`.
- `AppTabSurface.tsx:1` is `import Editor from "@monaco-editor/react"` and
  `GitDiffSurface.tsx:1` is `import { DiffEditor } from "@monaco-editor/react"`
  — static, top-of-module, in both.
- So the Monaco-bearing chunk must be fetched **and evaluated** before either
  module's body runs, and `AppTabSurface`'s `load()` — which does start on
  mount, before anything editor-shaped renders — cannot issue its read until
  after that.

The data flow P3 owns is complete and will start the request first the moment
the chunk boundary moves. P6 owns moving it.

**P4 has already done the equivalent split on the Git side and P3 has not done
it on the file side.** `features/git/useSharedGitDiff.ts` is the whole data half
of the diff surface — request, cache, cancellation, status reconciliation,
paint ticket — and it imports no Monaco. Hoisting it into a light loader shell
above the lazy `DiffEditor` boundary is a pure rendering change. The file side
has no such module: `AppTabSurface.tsx`'s `load()`, `reconcileBootstrap`,
`listingOpinion`, autosave controller and cancellation ownership all still live
in the Monaco-importing file, so P6 must extract them before it can move that
boundary. That extraction is a rendering-boundary change, not a data-flow
change, and stays inside P6's mandate.

### Shared-component landmines

- **Ownership is explicit and P6's mandate says not to cross it.** P3 owns
  file-open/watch/cancellation data flow; P4 owns Git diff fetching, cache and
  mutation reconciliation. In practice that means: do not change
  `useWorkspaceFiles.ts`, `directoryRequests.ts`, `watchLeases.ts`,
  `directoryState.ts`, `api.ts`, `repositoryStore.ts` or `useSharedGitDiff.ts`
  semantics while moving render boundaries.
- **`GitDiffSurface` no longer takes a client.** Its props changed from
  `client` + `onStatus` to `repositories: GitRepositoryStore`, and the
  sidebar's status reconciliation moved inside the store. A P6 change that
  reintroduces a per-surface client would undo P4's single-observation
  guarantee. Same for `GitSidebar`, which now takes one `git` object instead of
  five separate status/loading/error/refresh/accept props.
- **`AppTabSurface` and `GitDiffSurface` both hold paint tickets**
  (`perf/paintTicket.ts`) that span request start to committed pixels. Moving a
  Suspense boundary between the two halves will break the measurement unless
  the ticket travels with the request half, which is exactly the shape
  `useSharedGitDiff`'s `DiffPaint` already uses.
- **`AppTabSurface` also owns autosave.** The plan requires autosave/controller
  state to stay outside Suspense so a chunk transition cannot flush or unmount a
  dirty buffer; the controller currently sits in the Monaco-importing module,
  so the extraction has to keep it on the outer side of whatever boundary P6
  introduces.
- **`ExplorerRow.tsx` / `explorerWindow.ts` are new and windowed.** Roving
  focus is tracked as focus moves rather than by index, because precise patching
  moves rows under the cursor. Anything that re-parents or re-keys rows has to
  preserve that.
- **`explorer.expandToPaint` is published and read by nothing.** If P6 touches
  the probe surface, that is a dead key, not a signal.

### Protocol facts P6 will trip over

- `Operation::ReadFile` is an unconditional refusal. Any new code path that
  wants a file's bytes must use `OpenFileStream` on the bulk lane.
- `Operation::OpenFileStream = 44`, `Operation::GitDiffContent = 45`. The next
  free number is 46, and `crates/protocol/tests/roundtrip.rs` asserts that.
- `CAP_FILE_STREAM = 1 << 15` is required, not negotiated, on both the control
  and the bulk handshake.

### Carried-forward deferrals P6 inherits but does not own

Everything under "P3 deferrals" and "P3 round-6 residue" above, and the P4 rows.
The three most likely to surface during P6's work:

1. The Explorer's 150 ms expand-to-paint and 300 ms external-change-to-paint
   budgets, and the "no long task > 50 ms" outcome, are **unmeasured**. If P6
   runs a real browser, it is the first lane in this round that could collect
   them.
2. A **visible but unfocused** idle window still issues a periodic
   `resolveActiveRoot` probe (roughly three over a 60 s window), against a
   stated idle budget of zero. Gating on `document.hasFocus()` was tried and
   reverted because jsdom reports it `false` unconditionally; it belongs to
   whichever lane can run a real window.
3. `apps/host/src/service/git/path.rs::lossless_stat_component` panics on a
   stat component that does not fit. Neither P3 nor P4 fixed it.

## P6 — editor/startup interaction costs

Base: `1357e03` (the Wave 2 P3+P4 integrated head above).
Branch: `improvement/integration`, committed directly.
Final source commit: `e257ddc`. Every gate below ran against it; the head is
the commit adding this section, which changes no source.

| Commit | What |
| --- | --- |
| `d27764e` | One paint measurement for both editor surfaces (`perf/surfacePaint.ts`), with `useSharedGitDiff` rewired onto it. |
| `1321ebb` | The Monaco boundary moved inside the file and diff loader surfaces, plus the layout-observer lifetime, the shared language table and the debounced Markdown preview. |
| `af24bcb` | The font gate extracted to `startup/fontGate.ts` so its cost can be measured. |
| `641667e` | Round-1 review remediation. |
| `e257ddc` | Round-2 review remediation. |

### Per-bullet disposition

| Plan bullet | Disposition |
| --- | --- |
| Move the Monaco component boundary inside light file/diff loader surfaces so remote I/O starts before Monaco evaluation, and binary, oversized, disconnected and Markdown-preview-only paths never load Monaco. | **Done.** `useOpenFileTab.ts` is the file side's Monaco-free data half (read, cancellation, watch bootstrap reconciliation, autosave, paint ticket), extracted from `AppTabSurface.tsx` unchanged in substance; `FileEditor.tsx` and `GitDiffEditor.tsx` are the only modules importing `@monaco-editor/react`, reached through `React.lazy`. Bundle and test evidence below. |
| Keep autosave/controller state outside Suspense so chunk transitions cannot flush or unmount a dirty buffer. | **Done.** The controller lives in `useOpenFileTab`, above the inner `Suspense`. Covered by "keeps a dirty buffer and its pending save when the editor unmounts" in `app/editorChunkBoundary.test.tsx`: it types, removes the editor entirely, and asserts no write escapes, the dirty state survives, the pending save still commits the typed content, and the buffer returns intact. |
| Debounce/idle-schedule sanitized Markdown preview publication while editor and autosave state remain immediate. | **Done.** `features/files/markdownPreview.ts`: 120 ms debounce, then `requestIdleCallback` with a 400 ms deadline, falling back to a timer where the platform has no idle callback. First render still sanitizes synchronously, because opening a document has nothing to debounce. Unit tests plus a surface test asserting the editor value and the save state move on the keystroke while the article does not. |
| Retain the custom WKWebView-aware editor layout observer and disable Monaco's duplicate automatic layout **after coverage proves all resize/restore paths**. | **Half done, half deferred with reason.** Retained, and its lifetime fixed: the observer is now attached and detached by the editor component (`useEditorLayout`), where previously both surfaces detached only when the whole tab lifecycle ended, leaking one `ResizeObserver` per Markdown preview toggle. `automaticLayout: true` is **kept**. The coverage the plan makes the precondition is not obtainable here: `editorLayout.test.tsx` now covers mount-into-a-hidden-box, host resize, hidden-pane zero box, restore from zero, no-host fallback and detach-on-unmount, but the paths the custom observer exists for — WKWebView's undelivered first transition, window minimise/restore, display-scale change — need the packaged macOS lane, which is platform-blocked in this environment. Removing the second observer on jsdom evidence alone would risk exactly the 30x157-inside-740x690 blank editor it was written for. |
| Measure the existing font wait and Monaco capability use; keep the global font gate and the full Monaco surface. | **Done as measurement; both kept.** Numbers below. |

### Measurements

**Font wait** (`startup/fontGate.test.ts`, deterministic):

```
faces already usable        outcome "ready",       < 1 ms, no timer involved
faces never arrive          outcome "timeout",     exactly 2,000 ms, app still mounts
face wait rejects           outcome "unavailable", app still mounts
document.fonts absent       outcome "unavailable", no wait at all
```

The gate is kept. A terminal constructed before JetBrains Mono is usable
measures the fallback cell, and the tmux client grid is computed from that
cell; the 2 s bound is what stops a font that never resolves from becoming an
app that never starts. Real cold-launch font-wait duration on the packaged
macOS app is **platform-blocked** here (no app launch in this QA), so the
recorded numbers are the bound and the failure modes, not a device timing.

**Monaco capability use** (production build, `pnpm build`):

```
monaco chunk (eager, on first editor)      3,980,620 B   + 162,169 B CSS
lazily split language/mode chunks             92 chunks, 9,953,905 B total
language ids the app ever requests            11 (editorLanguage.ts, pinned by test)
editor capabilities relied on                 find/undo/keybindings (Monaco defaults),
                                              saveViewState per model path, diff editor
                                              with side-by-side + split resizing,
                                              custom theme, word wrap, readOnly
```

Kept whole, and the measurement is the argument for a *separate* follow-up
rather than a trim here: the 9.95 MB of language grammars is already
per-language dynamic chunks that are only fetched when a matching file is
opened, so trimming them buys nothing at startup. What is eager is the 3.98 MB
core-and-contributions chunk, and that is exactly what "do not silently remove
editor commands" protects. The plan's own deferral stands: any narrowing needs
a separately reviewed capability matrix.

**Layout observer**: two observers per editor by design (Monaco's
`automaticLayout` and the custom one). The custom one issues one `layout()` at
mount, one per host box change, and none for a zero box. Which paths are
covered deterministically, and which are not, is stated in the bullet table
above.

### Bundle evidence — required outcome "Monaco absent from the initial terminal-only closure"

Both builds are `pnpm build` at the repository root; the base was built from a
throwaway worktree at `1357e03` under `tmp/p6/base` (removed afterwards). The
closure is computed from `dist/index.html` outwards over static imports only.

| | base `1357e03` | P6 head | delta |
| --- | --- | --- | --- |
| initial entry chunks | 1 (`index-BOnnrj-l.js`) | 1 (`index-CNbJPsDh.js`) | — |
| **initial JS bytes** | **1,050,780** | **1,050,751** | **−29 B, −0.003%** (budget: no regression > 5%) |
| initial CSS | `index-BCAmSVu2.css`, 49,237 B | identical file and hash | — |
| Monaco in the initial closure | absent | absent | unchanged |
| `AppTabSurface` static closure | 5,116,322 B — `monaco` 3,980,167 + `index` 1,050,780 + surface 85,375 | 1,137,045 B — `index` 1,050,751 + surface 85,072 + `surfacePaint` 1,222 | **−3,979,277 B; the editor chunk is gone from it** |
| `GitDiffSurface` static closure | 5,041,229 B — includes the same `monaco` chunk | 1,061,412 B — `index` + surface 9,439 + `surfacePaint` 1,222 | **−3,979,817 B** |
| `FileEditor` / `GitDiffEditor` chunks | did not exist | 449 B / 556 B, each statically importing `monaco` (3,980,640 B) | the boundary |

The Markdown-preview-only path's chunk set is the `AppTabSurface` closure in
that table — `index`, the surface, `surfacePaint` — and excludes Monaco. That
the preview path never *renders* the lazy element (so never requests the chunk)
is asserted separately in `app/editorChunkAbsence.test.tsx`.

Monaco was already out of the initial entry closure before this change, since
both surfaces were already lazily imported. What changed is that it is now out
of the *loader* closure too, which is the part that stood between the tab
opening and the host request being sent.

### Required outcomes, stated honestly

| Outcome (plan, "Startup/editor") | Result |
| --- | --- |
| Monaco absent from the initial terminal-only closure | **Pass**, before and after; table above. |
| No fallback-atlas/metric change after first terminal paint | **Pass by construction, not by measurement.** The font gate is unchanged in behaviour and still blocks mount until the terminal's own faces are usable; no terminal, xterm, renderer or theme file is touched by this diff. A packaged-app atlas observation is platform-blocked. |
| Initial JS bytes do not regress > 5% | **Pass**, −0.002%. |
| First and repeated editor paint recorded | **Instrumented, not collected.** `workflow.file.editorPaint` now spans request start to committed pixels *including* the chunk fetch, so a first open and a repeated open are distinguishable at last; `editor.monacoRequest` now marks the moment the chunk is actually requested rather than a moment after it was already evaluated. Producing numbers needs a running app, which this QA explicitly excludes and for which phase 14 has no startup/editor lane. Recorded as uncollected rather than passed. |
| No autosave, find/undo, syntax, diff, or layout regression | **Pass at test level.** Autosave: the dirty-buffer-across-editor-unmount test plus the existing `AppTabSurface` suite (self-save echo, external reload, flush-on-close). Find/undo and syntax: no Monaco option or contribution changed, and the language table is pinned by test. Diff: the full `GitDiffSurface` suite (17 tests) passes unchanged in substance. Layout: `attachEditorLayout` unchanged, its call site moved into the editor component, with detach-on-unmount now covered. A packaged interactive pass is platform-blocked. |

### Review rounds

Two rounds, the cap the user set for this package. Each was a fresh
independent opus subagent, instructed to read
`/home/operator/dev/worktree-cli/skills/thermo-nuclear-code-quality-review/SKILL.md`
in full before anything else, given the diff range, the plan's P6 section and
the P3/P4 ownership fences — and nothing about the implementation's reasoning,
its self-assessment, or the previous round.

| Round | Task ID | Verdict | Findings | Disposition |
| --- | --- | --- | --- | --- |
| 1 | `a0de6f8ee671da819` | Request changes | (1) the consumer half of the paint protocol was still copy-pasted into both surfaces; (2) "what can this tab show" decided twice, in the hook and again as the surface's render ladder, with the editor size limit spelled out in both; (3) the Git ordering assertion was vacuous — one shared module marker fires once per registry, so by the time the Git case ran it could not fire again; (4) two comments named things that do not exist (a "capability probe", and a justification for `automaticLayout` that argued for the custom observer instead of for the deferral); (5) `createPaintReporter`'s identity rules are false when the perf probe is off, and `holding`/`abandon` overloaded an optional argument. Verified independently: the base's surface chunks did carry a static Monaco import and the new ones do not; entry bytes 1,050,780 → 1,050,758; the P3/P4 data modules byte-identical. | 1–5 all fixed in `641667e`: `useEditorPaint` for the consumer half; `OpenFileContent` union with the surface switching on it; per-editor-module evaluation markers, each verified non-vacuous by reintroducing a static import and watching the corresponding test fail; both comments rewritten; reporter API is `pending()`/`abandon()`/`discard(ticket)` with the inert-singleton condition documented. |
| 2 | `a6efc36bf76d2ffac` | Request changes | (1) blocker: the editor-generation reconciliation should be deleted rather than promoted, since the lazy boundary and per-tab keying make it redundant; (2) `useSharedGitDiff`'s paint state was rewritten and `abandon()` now also resets the committed generation — no live defect found, but untested; (3) the editor chunk is fetched strictly after the read resolves, serialising two independent units of work; (4) `GitDiffSurface` still had the two ladders its comment claimed to have merged, with an unguarded fallthrough; (5) `requestedLanguageIds` exists only to be asserted on; (6) `root!`, a dead `tabId` dependency, and three nested "Loading editor…" fallbacks; (7) no phase-14 artifacts for P6. Verified independently: the same bundle facts, and that the moved read/watch/reconciliation logic is a faithful line-by-line move. | 4, 5 and 6 fixed in `e257ddc`; 2 accepted as a stated risk and pinned by a new reporter test; 1 and 3 answered rather than changed, reasons below; 7 is what this ledger section records. |

**Why finding 1 was not acted on.** The generation triple is not made
redundant by the lazy boundary: `PaintTicket.afterPaint` completes two
animation frames after it is armed, and `held.surfaceGeneration ===
editor.mounted()` is what stops an `editor.paint` milestone being published
for an editor that unmounted inside that window — which a Markdown preview
toggle does routinely. It is also P4's measurement contract, carried into this
package by the merge agent's explicit instruction to keep the `notePaintable`
shape; retiring it is a change to what the Git diff measurement means, not to
lazy rendering or editor layout, and belongs to whoever owns that measurement.
Recorded as a residual finding rather than dismissed.

**Why finding 3 was not acted on.** Prefetching the editor chunk while the
remote read is in flight would overlap two serial costs, but the plan's
requirement is categorical: binary, oversized, disconnected and
Markdown-preview-only paths must never load Monaco. Size is not knowable until
the read answers, so every prefetch heuristic breaks the oversized case. The
chunk is also a local asset read in a packaged desktop app rather than a
network download, so the overlap being given up is smaller than the framing
suggests. A follow-up that wants it needs its own evidence and its own
decision about which of the two requirements gives way.

### Gate results

Run from `/home/operator/dev/muxflow` on the final head.

| Gate | Result |
| --- | --- |
| `pnpm --filter @tmux-agent-ide/desktop check` | Pass. |
| `pnpm --filter @tmux-agent-ide/desktop test` | Pass: **99 files, 863 tests** (from 93/835 at `1357e03`). The 28 new tests are the two chunk-boundary files, the paint reporter, the Markdown preview scheduler, the language table, the font gate, and the layout observer's lifetime. |
| `pnpm build` | Pass. Only the pre-existing Vite chunk-size advisory. Chunk table above. |
| Chunk-graph comparison against a `1357e03` build | Pass. Base built in a throwaway worktree under `tmp/p6/base`, removed afterwards; committed state untouched. |
| `git diff --check 1357e03..HEAD` | Pass. |
| `cargo fmt --all -- --check`, clippy | **Not run, and not required**: the diff touches no Rust. `git diff --stat 1357e03..HEAD` is entirely `apps/desktop/src/**` plus this ledger. |

### QA observations

The user-confirmed activity was bundle inspection plus focused Vitest, with no
app launch and no `cua`.

1. **Bundle inspection.** Numbers and chunk names in the table above. The
   initial entry chunk set is one JS chunk and one CSS file, and neither
   references Monaco; the file-tab and diff-tab loader chunks no longer do
   either; the two new editor chunks do, and are reached only through a
   dynamic import. Initial JS bytes moved by −29 B.
2. **Focused Vitest.** All four claims the QA asked for are asserted, and each
   was checked for vacuity by breaking the thing it claims:
   - *Read before editor evaluation* — `editorChunkBoundary.test.tsx` records
     the file read and the editor module's own evaluation into one ordered
     list and asserts `["openFile", "file editor module"]`, and the same for
     `["diff", "diff editor module"]`. Reintroducing a static import in either
     surface fails the corresponding assertion.
   - *Dirty buffer survives the editor going away* — types, removes the editor
     entirely, and asserts no write escaped, the dirty state survived, the
     pending save still committed the typed content, and the buffer came back
     intact.
   - *Preview publication debounced while the editor and save state are
     immediate* — asserts the editor value and the "Unsaved" chip move on the
     keystroke while the rendered article still holds the previous text, then
     that it catches up once timers advance.
   - *Binary, oversized, disconnected and preview-only paths never evaluate
     the editor module* — `editorChunkAbsence.test.tsx`, in its own registry,
     watching all three doors (both editor components and
     `@monaco-editor/react`). Adding a static import to `AppTabSurface` fails
     all six cases.
3. **Font-wait and layout-observer measurements** — recorded above. The
   packaged macOS launch that would produce real cold-launch timings, atlas
   observations and the resize/restore coverage the `automaticLayout` deferral
   waits on is **platform-blocked** in this environment, and is recorded as
   blocked rather than skipped.

No phase-14 artifacts were produced for P6: the harness has no startup/editor
lane, and building one requires launching the app, which this QA excludes.

### Residual findings, carried forward unfixed

| # | Finding | Why it is still here |
| --- | --- | --- |
| P6-R1 | The editor-generation reconciliation (`EditorSurfaceFacts`, `PaintTicket.expectSurface`/`surfaceGeneration`) could plausibly be replaced by "the next `onReady` while a ticket is pending", deleting ~60 lines and a field from the ticket contract. | Reviewer's round-2 blocker, answered above: the generation still guards the two-frame window between arming and painting, and the contract is P4's. A follow-up that owns the paint measurement could take it. |
| P6-R2 | The editor chunk is fetched only after the remote read resolves. | Deliberate; the alternative breaks a categorical requirement of this round. Answered above. |
| P6-R3 | `useOpenFileTab.ts` is ~400 lines and seven effects: read and cancellation, watch lease, bootstrap reconciliation, event subscription, autosave, and measurement. | It is a verbatim extraction of what one component held before, and splitting it further means separating the read from the bootstrap reconciliation that decides *about* that read — a data-flow change, which is P3's to make. |
| P6-R4 | First and repeated editor paint, the fallback-atlas observation, and the interactive no-regression pass are instrumented but uncollected. | Needs a running app; excluded by this QA and absent from phase 14. Not marked collected anywhere. |
| P6-R5 | The four `eslint-disable-next-line react-hooks/exhaustive-deps` comments in `useOpenFileTab.ts` are decorative: the repo has no ESLint configuration. | They match the existing convention in `useSharedGitDiff.ts` and `useWorkspaceGit.ts` and document the intent of each dependency list. Removing the convention is a repo-wide decision. |

Everything under the Wave 2 "Residual findings, carried forward unfixed" and
the P3/P4 deferrals above remains open; P6 touched none of it.

## Final integrated envelope — orchestrator verification at `98a8546`

Run by the orchestrating agent after P6 landed; commands per the plan's
"Integrated automated envelope", logs under the gitignored `tmp/wave2-envelope/`.

| Gate | Result |
| --- | --- |
| `pnpm check` / `pnpm test` (93 files, 863 tests) / `pnpm build` | pass |
| `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace --all-targets -- --test-threads=1` | pass |
| `tests/performance/runtime/run-idle-steady.sh 60` / `run-pause-probe.sh` / `run-stall.sh` | pass |
| `tests/performance/runtime/run-vt-parity.sh` | FAIL — environmental, not a Wave 2 regression: the identical failure (pauseContinue, 3 text rows, 0 attribute cells, all other quiesce points clean) reproduces at Wave 1 base `0faf78c` in the same environment. Wave 1 recorded this lane green at capture time; the current-environment failure predates every Wave 2 commit. Artifacts: `tmp/wave2-envelope/phase12-vt*.log`. |
| `pnpm test:transfers:smoke` (local + shaped Docker/OpenSSH transfer) | pass; evidence `tmp/evidence/phase7-local.20260817T065411Z-2996064/` and `tmp/evidence/phase7-ssh.20260817T065417Z-3004140/`. A first attempt failed only because the invoking shell exported a relative `CARGO_TARGET_DIR`; the harness was not at fault. |
| Enforced `run-perf.sh`, 60 s floods, local + shaped 100 ms Docker | pass, from a pristine temp worktree of `98a8546` (the enforced lane refuses the primary checkout's user-owned untracked `tests/acceptance/macos/evidence/`). Artifacts: `tmp/phase12-perf-20260817T065504Z-3019923/`. |

Shaped 100 ms Docker regression guard versus the final Wave 1 capture
(`tmp/phase12-perf-20260816T183453Z-2567910/`): echo p95 100.837 ms (was
100.804), window switch 100.720 ms (100.777), new tab 211.774 ms (211.671),
new workspace 212.072 ms (212.444), resize 104.102 ms (104.344), typing during
flood 159.728 ms (160.222), sustained 11.085 MB/s (11.093), app overhead vs raw
ssh+tmux 0.323 ms; zero flood/connection resyncs and zero sequence gaps in both
lanes. Wave 2's shared-path changes did not move the terminal/workspace
envelope. The lane's MISS rows (renderer expand/change paint, whole-app idle
polling) are the known harness gaps recorded in the deferrals above; no shaped
journey lane exists for the new Explorer/file/Git paths, so their improvement
remains proven by request/process counts, not milliseconds.

The plan's final packaged-app `cua` QA stage was explicitly skipped by the
user for this wave and is recorded as skipped, not passed.
