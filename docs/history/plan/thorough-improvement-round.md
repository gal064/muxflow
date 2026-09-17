# Thorough performance and reliability improvement round

## Context

The application is a Rust/Tauri desktop IDE whose authoritative state lives in
tmux. It already has strong correctness and performance evidence from phases 12
and 13, but the current code was assembled quickly and several hot paths have
multiplicative work, while several orchestration and admission paths can enter
incoherent states. This round should make the app noticeably snappier under
terminal floods, large workspaces, resizing, file watching, and first editor
use; keep idle resource use near zero; and remove concrete race/ownership bugs
without changing product behavior.

The existing untracked `tests/acceptance/macos/evidence/` directory belongs to the user
and must remain untouched.

## Primary deployment path

The primary product path is the desktop app running locally on macOS while it
controls a remote PMAX Linux server over SSH. Local-host measurements remain
useful regression controls, but implementation priority and acceptance are
weighted toward the established remote connection:

1. connect/reconnect and SSH control-master reuse;
2. create, open, and switch workspaces/tabs;
3. open files and navigate/expand Explorer directories;
4. Git status, diff navigation, and mutations;
5. terminal flood/idle efficiency and local-only polish.

For those first four paths, optimize the whole latency chain: network round
trips, remote tmux/Git/filesystem processes and syscalls, connection reuse,
request/payload size, caching and safe prefetch, cancellation/stale-scope
handling, and immediate perceived feedback while authority is pending. The app
must never claim a remote mutation succeeded before the host confirms it.

## Repository map and current baseline

- `apps/desktop/src`: React shell, xterm renderer, Explorer, Git, agent UI, and
  in-app performance probes.
- `apps/desktop/src-tauri`: Rust bridge/transport, Tauri IPC, persistence, and
  local transfer managers.
- `apps/host`: authoritative daemon, tmux control, filesystem/Git services,
  agents/hooks, and request dispatch.
- `crates/protocol`: length-prefixed protobuf transport.
- `crates/tmux-control`: tmux discovery/parser and pane replay/resource state.
- `tests/performance/runtime` and `tests/integration/agent-status`: performance, recovery, parity, idle,
  agent-status, and packaged-app QA fixtures.

Recorded host/link results are already healthy: local echo p95 is about 0.26
ms, local create-window p95 about 37 ms, local create-session p95 about 46 ms,
local sustained output about 23 MB/s, and the host emits zero idle frames. The
current gaps are whole-desktop idle work, browser-side terminal CPU/allocation,
Explorer 4,096-entry render/change latency, watcher multiplicity, startup
milestones, and transfer admission saturation.

## Guardrails

- Preserve tmux authority, post-mutation discovery, topology generations,
  sequence ordering, scoped reseed/recovery, and exact hide/checkpoint/tail
  semantics.
- Preserve one xterm write in flight and immutable ownership until its callback.
- Preserve filesystem root capabilities, descriptor-relative/no-follow path
  safety, transfer journals, cleanup truth, and monotonic terminal outcomes.
- Preserve Phase 13 consent, captured host identity/targets, hook evidence, and
  merge-only configuration changes.
- Do not trade correctness for lossy event coalescing, relaxed generation
  checks, or blanket removal of tmux discovery.
- Do not pursue tiny memory-only savings. Optimize multiplicative CPU, I/O,
  process creation, allocation volume, interaction latency, and unbounded queues.

## Stage 0 — measurement foundation and baseline (sequential)

Create `tests/performance/optimization/` as the opt-in improvement-round harness, reusing
`apps/desktop/src/perf/probe.ts`, `tests/performance/runtime/setup-qa.sh`, and the existing
isolated tmux/SSH fixtures. It must record toolchain/commit/dirty/display context
and write machine-readable artifacts under `tmp/phase14/<label>/`.

Add deterministic, non-wall-clock counters/fixtures for:

1. Terminal frontend event count, copied bytes, queue operations, callbacks,
   frame count, and byte-exact output at 64 B, 1 KiB, and 64 KiB chunks.
2. `PaneResourceStore` full-accounting scans, LRU operations, retained byte/pane
   totals, eviction, and reveal parity at increasing pane/chunk counts.
3. Desktop-to-host requests/events, React long tasks, and process CPU/RSS during
   60 seconds of visible and background idle.
4. Explorer expand-to-paint and external-change-to-paint using the existing
   4,096-entry `wide/` fixture, including directory list/watch counts and DOM row
   high-water.
5. Native Git watcher/status-process counts with 32 consumers of one repository.
6. Module start, first React commit, font ready/timeout, first terminal paint,
   Monaco request, and first/second editor paint.
7. Transfer engine active/queued counts and terminal outcomes under a full queue.
8. Tauri bridge ingress bytes, channel-send bytes/duration, JS admission lag,
   and RSS while the WebView consumer is deliberately stalled.
9. Packaged-workflow spans bound to exact connection/session/window/pane targets:
   connect, create/select workspace, create/select tab, root paint, directory
   expand paint, file first-content/editor paint, Git panel/diff paint, and Git
   mutation acknowledgement. The current Phase 12 driver omits the frontend's
   second create-tab selection and workspace visibility restatement, so it is
   not sufficient evidence for these journeys.

Capture `before` evidence from the instrumented baseline before merging any
optimization. The mandatory baseline is the existing shaped 100 ms SSH/Docker
fixture; the local lane is its control. Add operation-level request, response,
payload-byte, remote-process, cache-hit, cancellation, and interaction spans for
remote connect/control-master establishment, workspace/tab create/open/select,
file open, Explorer list/watch/expand, and Git status/diff/mutations. A
Mac-specific launch that cannot run in the current environment is `BLOCKED`,
not silently replaced by Linux evidence. Instrumentation must be inert unless
explicitly enabled and must not affect normal behavior.

## Stage 1 — parallel correctness packages

Create one integration branch and isolated worktrees under `/tmp`; every branch
starts from the Stage 0 baseline. One implementor owns each conflict cluster.

### C1 — shell target and recovery coherence

Paths: `apps/desktop/src/features/shell/useShellCommands.ts`, its tests,
`apps/desktop/src/app/App.tsx`, `AppDialogLayer.tsx`, `WorkspaceSidebar.tsx`, and
App integration tests.

- Extract pure discriminated command-target resolution. An explicit terminal,
  workspace, agent, or app-tab target suppresses unrelated ambient fallbacks.
- Replace the split recovery payload/boolean with one recovery state so scope
  invalidation cannot leave `modalOpen` true without a rendered dialog. Use
  durable connection identity rather than topology generation for a still-valid
  offer.
- Keep sidebar drag geometry transient/rAF-coalesced and commit persisted width
  or ratio once on pointer-up/cancel.
- Add a narrow memo boundary around the terminal workspace and stable command
  dispatch only when Stage 0 render counters show unrelated root updates reach
  terminal panes. Do not deep-compare topology.

### C2 — hook-review and lifecycle durability

Paths: `apps/desktop/src/features/agents/useAgentHostSetup.tsx`,
`AgentHookWorkflow.tsx`, `apps/host/src/hook.rs`,
`apps/host/src/service/agents/fallback.rs`, agent dispatch/runtime, and tests.

- Do not dismiss host setup until asynchronous hook-review loading succeeds;
  keep the captured host/targets and a retryable error state on failure.
- Introduce explicit hook ingest dispositions (`applied`, permanent/duplicate,
  retryable). A retryable live rejection writes exactly one fallback event, and
  retryable replay remains queued rather than being deleted.
- Keep malformed/duplicate entries idempotently discardable and retain all
  redaction/consent/config-ownership rules.

### C3 — protocol framing and exhaustive host operation policy

Paths: `crates/protocol/src/lib.rs`, protocol tests,
`apps/host/src/service/requests/{dispatcher,file_ops,filesystem_dispatch,agent_dispatch}.rs`,
and policy tests.

- Fix `FrameAccumulator` so the size limit applies to the current advertised
  frame, not aggregate bytes containing later valid frames.
- Centralize operation family, mutation/read-only access, control/bulk lane, and
  scheduling classification in one exhaustive policy. Preserve unknown numeric
  operations as unknown rather than aliasing `Unspecified`.
- Classify `AgentHostNaming` as a mutation and prove a read-only connection
  cannot apply or remove it.
- Add compatibility tests for every generated operation. Do not rewrite the
  protobuf into a new `oneof` in this round.

### C4 — transactional transfer admission and destination ownership

Paths: `apps/desktop/src-tauri/src/connection/files/{scheduler,download_manager,upload_manager,editor_manager,local_destination,download_naming,transfer_event}.rs`
and focused tests.

- Add an exclusive RAII destination lease for the exact final publish leaf.
  Concurrent rename/save choices must either reserve distinct leaves or reject
  deterministically; release only the owning lease.
- Make scheduler admission transactional. No `queued` event, cancellation
  entry, staging guard, or destination reservation may escape if enqueue fails;
  after successful insertion `queued` must precede `running`.
- Roll back worker-spawn failure instead of panicking and terminalize the job
  exactly once.
- Keep the existing two-active/128-queued bound, digests, no-follow checks,
  collision confirmation, publication outcome, and cleanup evidence.

## Stage 2 — parallel high-confidence performance packages

These branches start after Stage 1 primitives are integrated, or rebase onto
them when they use the same types.

### P1 — terminal frontend hot path

Paths: `TerminalEventHub.ts`, `TerminalPane.tsx`, `TerminalRenderer.ts`, terminal
`api.ts`, `TerminalStateCache.ts`, and terminal tests.

- Replace per-pane global epoch listeners with a dedicated epoch subscription
  that is invoked only for accepted epoch events.
- Replace hidden-backlog array spread and renderer `Array.shift()` with
  appendable/head-index queues whose occasional compaction is bounded.
- Establish one explicit byte-ownership boundary: avoid decoder plus scheduler
  double copies while retaining immutable data until xterm acknowledges it.
- Reuse already-encoded snapshot byte length for cache accounting.
- Preserve seed/resource/diagnostic boundaries, LRU eviction, generation
  checkpoints, overflow signaling, seal/drain behavior, and output parity.

### P2 — host pane resources and tmux fork count

Paths: `crates/tmux-control/src/replay.rs`,
`apps/host/src/service/terminal/stream.rs`, `snapshot.rs`, `tmux_actions.rs`, and
tests.

- Maintain exact incremental retained-byte, retained-pane, journal-byte, and
  LRU accounting. Remove full-store/journal scans and `VecDeque::retain` from
  every output-chunk append.
- Batch/move hidden seed and replay data where ownership permits, without
  changing visible delivery.
- Attempt batched authoritative discovery first and make the no-server identity
  fallback lazy. Probe bootstrap identity only for create-session behavior.
- On an established SSH profile, record and reduce the exact remote tmux fork
  and protocol-round-trip count for create/open/select workspace and tab. Reuse
  the live bridge/control master; no ordinary action may create a fresh SSH
  process or repeat control-master validation.
- Collapse journeys that are semantically one operation: create-and-select a
  terminal tab under one topology transaction, and let a successful
  `SelectSession`/create-session acknowledgement satisfy the matching terminal
  session assertion for that client instead of immediately sending a second
  selection RTT. Reconnect and new client IDs still reassert ownership.
- Move the input barrier before the final pre-action discovery and pass that
  fresh snapshot into execution, retaining one authoritative precheck and one
  postcheck rather than two prechecks. Suppress only a known duplicate dirty
  reconciliation closed by the same action epoch.
- Skip membership marker/control writes only for exact no-op membership and
  group panes by session in one pass.
- Keep pre/post mutation consistency discovery beyond the demonstrably
  redundant identity fork.

### P3 — precise filesystem watch propagation

Paths: `apps/desktop/src/features/files/{api,types,useWorkspaceFiles,ExplorerTree}.ts(x)`,
`apps/host/src/service/filesystem/watch_service.rs`, and tests.

- Preserve mapped metadata/delete events and authoritative bootstrap/rescan
  listings through the desktop adapter instead of reducing them to invalidation.
- Patch complete cached listings for precise events; replace from authoritative
  snapshots; perform one recovery list only for overflow, gaps, incomplete pages,
  or mapping uncertainty.
- Maintain a keyed lease map and diff desired reachable expanded directories so
  toggling one folder acquires/releases only changed watches. Release invisible
  descendant watches while retaining UI expansion memory.
- Make polling fallback per failed target with backoff/re-registration, leaving
  healthy native watches idle. Remove the 400 ms unchanged-snapshot loop.
- Isolate Explorer rows behind memoized stable handlers. If the instrumented
  4,096-entry lane still exceeds 150 ms or creates a long task, add accessible
  windowing with overscan while preserving roving focus, selection, context
  menus, and scroll-to-item behavior.
- Treat watch bootstrap as the initial remote listing so an expansion does not
  pay a separate list round trip. Cache complete listings by connection/root
  token/directory generation, show a valid cached revisit immediately, and
  revalidate in the background. Prefetch only bounded, high-confidence targets
  (the selected workspace root and a just-opened directory's first page), and
  cancel or ignore prefetch on connection/root/generation changes.
- Start remote file I/O before Monaco/editor evaluation; surface a lightweight
  loading shell immediately, keep one owning cancellation token across control
  and bulk phases, and avoid retransmitting metadata or content already supplied
  by the authoritative watch/list response.
- Replace metadata/preflight/per-chunk pull for editor opens with one bulk
  `OpenFileStream` request. The host opens one descriptor-bound generation,
  returns metadata/classification once, and streams bounded chunks continuously.
  A warm text or image open costs one protocol RTT plus transfer time, not
  `2 + ceil(bytes/1 MiB)` or an extra text probe. Bind server identity, epoch,
  root token, path capability, generation, cancellation, and terminal response
  before a bulk bridge returns to the pool.
- Start initial read and parent-watch bootstrap together after subscribing. Use
  the bootstrap entry generation to accept the first completed read or reload
  only on a real mismatch/deletion; never unconditionally abort and reread.
- Make pagination snapshot-backed and bind opaque page tokens to server/root/
  directory identity and generation. Later pages reuse the bounded ordered
  snapshot instead of rescanning and stating the entire remote directory.
- Add real cancellation from renderer operation ID through Tauri request ID to
  bounded host list/open loops. Collapse, scope/root replacement, and superseded
  previews stop remote read work; mutations keep non-replay semantics.
- Replace the unconditional two-second active-root pipeline with a foreground/
  relevant-surface backstop and a narrow generation-checked pane-CWD probe.
  Unchanged roots emit no duplicate payload; replacement invalidates every
  path/list/content cache.

### P4 — shared Git observation and sidebar rendering

Paths: `apps/desktop/src/features/git/{api,useWorkspaceGit,GitDiffSurface,GitSidebar}.ts(x)`,
`apps/desktop/src-tauri/src/connection/git.rs`,
`apps/host/src/service/git/{watch,status,path,diff,mutation}.rs`,
`apps/host/src/service/git.rs`, relevant protocol fields, and tests.

- Share one repository watch/status stream among sidebar and matching diff tabs;
  saved/cross-root tabs retain an explicit fallback.
- Deduplicate worktree/git/common directory watch paths and multiplex subscribers
  so 32 consumers create one native watcher and one coalesced status refresh.
- Separate commit-form state from memoized status groups and equality-bail the
  same focused row so typing does not rebuild up to 1,000 rows.
- Remove redundant status subprocesses only where existing porcelain output
  provides the same authority; retain authoritative binary handling in diff.
- Cache repository identity and the latest authoritative status/diff metadata by
  connection/root/generation; opening the panel or a second matching diff must
  reuse that state instead of starting another remote discovery/status pipeline.
  Coalesce identical in-flight requests and cancel/ignore them on scope change.
- Keep remote mutations pessimistic for authority but make their pending target
  visible immediately. On acknowledgement, consume the resulting authoritative
  refresh rather than launching duplicate status work.
- Add one repository coordinator keyed by client/server/epoch/root token/
  repository identity. It owns the bootstrap promise, native watcher,
  subscribers, in-flight status, cached static capabilities, and mutation
  reconciliation. Native-watch fallback retries with capped backoff and does not
  run a full Git pipeline every 750 ms forever.
- Batch the three static `rev-parse` queries on a cache miss and derive branch/
  HEAD from authoritative porcelain status where equivalent. Preserve the
  visible binary badge until one-process equivalence is proven.
- A matching diff opens in one RTT from cached/watch status and its response
  carries validated authoritative status. Remove the prerequisite status RTT,
  duplicate diff watch, and unused patch body. Route large old/new bodies over
  the persistent bulk lane so terminal control is not head-of-line blocked.
- Consume mutation-returned authoritative status. Stage/unstage/commit performs
  one request and one post-command status pipeline; diff actions request only a
  remaining diff, not a new status-to-diff chain. Scope-check completions and
  retain cancel-before-registration tombstones for fast aborts.

### P5 — safe Tauri bridge and input-path wins

Paths: `apps/desktop/src-tauri/src/connection/{event_frame,bridge,transport}.rs`,
`connection.rs`, and tests.

- Match/move protocol payloads without cloning them merely to detect responses;
  encode terminal and pane-resource frames directly into one final allocation.
- Move resize onto an ordered async/blocking lane so input flush and remote
  response waits cannot block the WebView command thread; final size wins.
- Add a byte budget as well as the 512-message input bound, including the legacy
  string command, and synchronously refuse retryable excess without displacing
  accepted input.
- Make reconnect backoff cancellable so stopped clients promptly release workers
  and SSH leases.
- Move control-master establishment/revalidation off the synchronous command
  path, coordinate per socket rather than holding a process-global registry lock
  across `ssh` subprocesses, and eliminate the duplicate initial `ssh -O check`.
  Established-profile actions, file operations, and Git operations must reuse
  the same master/bridge and may not serialize unrelated profiles behind it.
- Keep a zero-lease master warm for its configured persist window rather than
  immediately issuing `ssh -O exit`; explicit app exit still closes it. Pass the
  acquired socket/lease into bridge startup so connect/reconnect validates once.
- Pipeline ClientHello and Subscribe while validating the hello before admitting
  the returned snapshot. Cache `tmux -V`/`git --version` for the daemon lifetime
  and emit local `connecting` state before network work.
- Use Stage 0 evidence for Tauri consumer backpressure. If retained IPC bytes/RSS
  grow with a stalled WebView, implement a bounded acknowledged byte window that
  pauses bridge reads and lets existing pipe/host flow control engage. It must
  never drop/reorder frames or synthesize false gaps. If evidence does not show
  growth, keep the current delivery model and retain the instrumentation.

### P6 — editor/startup interaction costs

Paths: `apps/desktop/src/main.tsx`,
`features/shell/{AppTabSurface}.tsx`, `features/git/GitDiffSurface.tsx`,
`features/files/{monaco,markdown,editorLayout}.ts`, and tests.

- Move the Monaco component boundary inside light file/diff loader surfaces so
  remote I/O starts before Monaco evaluation and binary, oversized, disconnected,
  and Markdown-preview-only paths never load Monaco.
- Keep autosave/controller state outside Suspense so chunk transitions cannot
  flush or unmount a dirty buffer.
- Debounce/idle-schedule sanitized Markdown preview publication while editor and
  autosave state remain immediate.
- Retain the custom WKWebView-aware editor layout observer and disable Monaco's
  duplicate automatic layout after coverage proves all resize/restore paths.
- Measure the existing font wait and Monaco capability use. Keep the global font
  gate and full Monaco language/contribution surface unless cold-launch or
  first-editor evidence justifies a narrower follow-up; do not risk terminal
  geometry or silently remove editor commands in this round.
- P3 owns file-open/watch/cancellation data flow and P4 owns Git diff fetching,
  cache, and mutation reconciliation. P6 may change only lazy rendering and
  editor layout in shared components after P3/P4 land.

## Integration and merge order

Use a dedicated merge agent after implementors finish. It must inspect every
branch diff and test result before merging; no blind merge-all.

### Mandatory thermo-nuclear branch gate

Every implementation agent, including Stage 0 and all later waves, must spawn
independent branch sub-reviewers before its work is eligible to merge. Every
reviewer must first read the complete exact instructions at:

`/home/operator/dev/worktree-cli/skills/thermo-nuclear-code-quality-review/SKILL.md`

The minimum is one full review. Use two or three independent rounds for
substantial, architectural, cross-cutting, concurrency-sensitive, or
invariant-heavy changes, and whenever an earlier round finds meaningful issues.
Review is a blocking quality gate, not advisory:

1. Review the complete branch diff against its recorded base.
2. Fix every high-conviction structural, abstraction, modularity, file-size,
   boundary, type-contract, spaghetti-growth, and maintainability finding.
3. Rerun the relevant focused tests, full affected suite, checks/build, and
   performance/correctness fixture.
4. Repeat with a fresh reviewer until no high-conviction blocker remains.
5. Commit the fixes and report reviewer task names/IDs, findings and verdict per
   round, exact fixes, test commands/results, and final clean commit.

The merge agent must verify this evidence for each branch and reject any branch
that only passes tests or treats review findings as optional.

1. Stage 0 measurement foundation and saved `before` artifacts.
2. C3 protocol/policy, then C4 transfer admission.
3. C1 shell and C2 hook reliability.
4. P2 pane/tmux host performance, followed by P5 SSH/Tauri connection reuse.
5. P3 remote filesystem/Explorer/file-stream protocol and P4 remote Git. They
   start after C3/C4; P3 owns file bulk protocol while P4 owns Git control/bulk
   protocol, with schema additions coordinated by the merge agent.
6. P1 terminal frontend after the primary remote interaction paths are green.
7. P6 editor/startup, with file-I/O-before-editor ordering integrated with P3.
8. Resolve only real overlap, run formatting, and remove no user evidence.

The merge agent must leave one integrated branch/worktree and a ledger mapping
each planned item to merged commit, deferred-with-evidence, or rejected-with-
reason. It must not claim completion for conditional work whose metric was not
collected.

## Verification

### Focused gates per branch

- TypeScript: `pnpm --filter @tmux-agent-ide/desktop check` and focused Vitest
  files, followed by the full desktop suite for shared UI/state changes.
- Rust: `cargo fmt --all -- --check`, focused crate/package tests, then
  `cargo clippy --workspace --all-targets -- -D warnings` for shared protocol or
  host changes.
- Every correctness regression gets a deterministic test for the exact failure:
  explicit target vs ambient tab, orphan recovery modal, failed hook-review
  handoff, retryable hook persistence, read-only naming, duplicate destination,
  queue-full ghost transfer, worker-spawn rollback, and concatenated maximum
  frames.

### Required after/before outcomes

- Established shaped-SSH connection: one persistent bridge/control master;
  zero new master or redundant `ssh -O check` per workspace/tab, file, Explorer,
  or Git action; reconnect reuses a valid master and stale completions cannot
  update a replacement scope.
- Remote workspace/tab actions: record exact request, RTT, and tmux-process
  counts; remove the eager identity fork; shaped-SSH p95 must not regress and
  must remain inside the existing 500 ms create / 150 ms switch budgets, with
  app overhead versus raw SSH+tmux <=10 ms.
- Remote workspace/tab structure: create-tab is one protocol request/topology
  transaction, not create then select; workspace selection is not immediately
  restated; rapid A-to-B-to-C ends on C and late A/B success cannot update a
  replacement scope.
- Remote file open: begin the host request before editor chunk evaluation; one
  owning cancellation spans control and bulk work; time-to-loading-shell is one
  local frame; time-to-first-content and transferred bytes do not regress; a
  cancelled/stale open publishes no late buffer. A warm small text/image open is
  one bulk request/RTT plus transfer time; 10 MiB does not form a per-MiB RTT
  staircase; watch bootstrap causes no unconditional second read.
- Remote Explorer: one watch-bootstrap round trip for first expansion and no
  redundant list; cached revisits paint locally then revalidate; one-file remote
  changes patch the listing without a second full scan/list payload. Collapse is
  one unwatch; pagination after cache establishment enumerates/stats at most one
  page plus a small constant.
- Remote Git: one repository watcher and one coalesced authoritative status
  pipeline; opening matching diffs reuses repository/status identity; each
  mutation produces one pending UI transition and one authoritative refresh,
  with no duplicate status subprocess burst. Warm panel is zero requests; a
  matching diff is one RTT; stage/unstage/commit is one request and one
  post-command pipeline; no unchanged 750 ms fallback polling.

- Terminal frontend: byte/callback order exact, no new overflow/resync/gap; tiny
  chunk CPU/allocation at most 70% of baseline; large-chunk regression at most
  10%; 8x bytes scales at most 10x.
- Pane resources: zero full-store accounting scans and zero linear LRU retains
  per append; retained budgets exact; 1,024-pane per-op time at most 2x the
  32-pane case and at most 50% of baseline.
- Explorer: 4,096-entry expand-to-paint p95 <=150 ms and external-change-to-
  paint p95 <=300 ms; no list/watch storm, duplicate/missing row, scroll/focus
  loss, or long task >50 ms.
- Git: one native watcher/status refresh stream per repository regardless of
  consumers; final release/reconnect leaves no task.
- Idle: zero periodic desktop-to-host requests, seeds, topology pushes,
  resyncs, Git refreshes, or directory lists after warmup; no >50 ms long task;
  CPU <=1% of one core; RSS growth <=8 MiB and no >10% regression.
- Startup/editor: Monaco absent from the initial terminal-only closure; no
  fallback-atlas/metric change after first terminal paint; initial JS bytes do
  not regress >5%; first and repeated editor paint recorded with no autosave,
  find/undo, syntax, diff, or layout regression.
- Transfer admission: exactly two active and at most 128 queued; overflow
  rejects promptly with no ghost row/reservation/partial; each accepted job has
  exactly one terminal outcome.
- Stalled WebView: ingress remains ordered and exact. If baseline grows without
  bound, the candidate RSS/retained-byte curve must plateau at the configured
  window.

### Integrated automated envelope

Run after all merges:

```sh
pnpm check
pnpm test
pnpm build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
ADE_PHASE12_SKIP_DOCKER=1 ADE_PHASE12_ENFORCE=1 \
  ADE_PHASE12_FLOOD_SECONDS=10 bash tests/performance/runtime/run-perf.sh
bash tests/performance/runtime/run-idle-steady.sh 60
bash tests/performance/runtime/run-pause-probe.sh
bash tests/performance/runtime/run-vt-parity.sh
bash tests/performance/runtime/run-stall.sh
pnpm test:transfers:smoke
```

Then run the shaped 100 ms Docker lane with a 60-second flood and the remote
workspace/tab, file/Explorer, and Git interaction journeys, storing `after`
phase-14 results beside the shaped-SSH baseline. Required metrics may not be
reported as `MISS`.

### Final packaged-app QA with `cua`

A fresh `sol`/high QA agent owns this final stage. It must use the installed
`cua` CLI (not the stale `cua-driver`/`cua-virtual-driver` names), window-scoped
captures, fresh element tokens after every state change, and isolated
`tests/release/setup-cua.sh`/phase-12 fixtures. Store artifacts under
`tmp/final-qa/<UTC-run-id>/`.

The journey must select the disposable shaped Linux SSH profile as its primary
lane and cover cold launch/connect to Live/nonblank terminal; control-master
reuse; remote workspace/tab create/open/switch; exact typed bytes
verified with `tmux capture-pane`; split/zoom/window switch/search/scroll;
4,096-entry remote Explorer and external create/change/delete; remote file open,
cancel, autosave, external refresh, and reopen; shared remote Git watcher plus
diff/stage/unstage/commit; bounded Docker-SSH transfers including queue/cancel
cleanup; 60-second visible and background idle; 1280x800 and 860x600 layout;
forced SSH reconnect/recovery; and app restart with persisted remote
workspace/document/panel state.

Record cold and warm results separately. The shaped host asserts remote tmux,
filesystem-stat, and Git process counts; the packaged macOS lane is mandatory
for WKWebView/Tauri byte-copy, long-task, socket reuse, pending-feedback,
sleep/wake, and network-roam evidence. A Linux-only run reports those Mac rows as
platform-blocked rather than passed.

The final QA report must distinguish pass, fail, and platform-blocked. Virtual
X11 cannot prove native Wayland, macOS TCC/sleep/notifications/Finder/IME, or
mixed-scale display behavior; those remain explicit platform lanes rather than
being reported as passed.

## Explicit deferrals

- No wholesale `App.tsx` rewrite or state-management framework migration.
- No protobuf request hierarchy rewrite.
- No removal of authoritative tmux postcondition discovery.
- No lossy Tauri event dropping/coalescing.
- No Monaco language/contribution trimming or global font-gate removal without
  evidence and a separately reviewed capability matrix.
- No daemon-wide transfer coordinator rewrite in this round; admission and local
  destination ownership are fixed first, while cross-connection cleanup design
  remains a future protocol change.
