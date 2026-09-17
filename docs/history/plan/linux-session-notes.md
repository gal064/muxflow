# Linux session notes — `perf-improvements`

All five items of the brief are implemented, tested and committed.

**No performance claim in this branch is verified.** Everything here is "code
plus tests, green on Linux, awaiting Mac measurement". The measurement harness
is macOS-only and nothing was run against a live app.

Branched from `perf-measurement` (c65fd63). Note both `plan/perf-next-steps.md`
and `tests/performance/benchmark/` exist only on that branch, not on
`improvement/integration`.

## QA, run before every commit

| gate | result |
| --- | --- |
| `cargo test -p tmux-ide-host` | green (362 lib + 20 suites) |
| `cargo test -p tmux-agent-desktop --lib` | 226 passed, 0 failed |
| `cd apps/desktop && npx vitest run` | 908 passed / 102 files (882 at baseline) |
| `npx tsc -b` | clean |
| `cargo clippy -p tmux-agent-desktop -p tmux-ide-host --all-targets` | 2 warnings, both pre-existing, neither in a touched file |
| `cargo build --release -p tmux-agent-desktop` | compiles |
| `… --release --features perf-log` | compiles |
| frontend build + `dist/assets` | entry 1,054.11 kB, monaco separate at 3,980,635 |

Every new test was mutation-checked: the code it pins was deliberately broken,
the test confirmed red, the break reverted. All caught. Details
per item below and in each commit message. 23 mutations across the five items.

### One environment note worth knowing

Most of this session was spent unable to run anything: `/tmp` is a 14 G tmpfs
mounted with `usrquota`, the user's quota was exhausted, and Claude Code
captures command output to `/tmp/claude-1000/...`. Every command — including
`echo` — returned exit 1 with empty output, because the *capture* failed, not
the command. Exit codes still propagated (`exit 42` → 42), which is what
eventually identified it.

The practical consequence for anyone reading the history: an early
`cargo test -p tmux-ide-host` was recorded as a failure and **was not one**. The
host baseline is green and was re-established after `/tmp` was cleared.

## 1. Monaco idle preload — `95eddc7`

**What changed.** New `apps/desktop/src/startup/editorPreload.ts`:
`preloadEditorChunk()` does the dynamic `import("../features/files/FileEditor")`
and swallows failures; `scheduleEditorPreload()` defers it to
`requestIdleCallback` with a 3 s deadline, falling back to `setTimeout(…, 0)`
where idle callbacks do not exist. Called from `main.tsx`'s `mount()` after
`render`.

**Why bootstrap and not a surface.** `editorChunkAbsence.test.tsx` pins that a
binary file, an oversized file, a Markdown preview and a disconnected tab never
reach the editor bundle. A preload hung off a surface breaks that for all four.
Bootstrap has no such claim to keep. The test passes unmodified.

**Bundle budget — measured, not assumed.** Built twice, once with the `main.tsx`
change stashed:

| | before | after |
| --- | ---: | ---: |
| entry chunk | 1,051,468 B | 1,051,991 B |
| monaco chunk | 3,980,635 B | 3,980,635 B |

+523 bytes to the entry (the preload module itself), monaco byte-identical and
still separate. The dynamic import did not enter the entry closure.

**Pinned by** `src/startup/editorPreload.test.ts`, 7 cases. **Mutation-checked,
6/6 caught**: preloading eagerly, dropping the deadline, cancel not withdrawing
the idle callback, cancel not withdrawing the fallback timer, removing the
fallback, letting a failed fetch reject.

**Mac must confirm:** decomposition.md's ~490 ms Monaco segment plus its 6–100 ms
content gate disappearing from the *first* open of a session, **and no startup
regression** — `startup.firstReactCommit` and terminal first paint must not move,
since the preload is scheduled from `mount()`. Steady-state opens should be
unchanged.

## 2. Bulk lease pre-warm — `9f01a12`

**What changed.** `bulk_pool.rs` gains `prewarm_bulk_bridge()`, called from
`bridge.rs` immediately after the connection publishes as live and writable. It
establishes one bridge and leaves it in the idle pool by dropping the lease —
`BulkLease::drop` already pools a clean, live, uncancelled bridge, so there is
no second establishment path to keep in step with the first. Runs on its own
thread: the caller is the terminal bridge's reader loop, which carries every
keystroke.

**The read-only/settling gate.** `BulkBinding::validate()` is the guard, and it
is re-checked on the pre-warm thread, not only at capture — the binding is taken
on the bridge thread and the connection can go read-only in between. Its failure
string is verbatim the one from decomposition.md's "Observed once"
(`bulk job is not bound to a writable live control connection`). A pre-warm has
no user behind it, so it gives up silently and never surfaces that state.

**Deliberate narrowing — read this.** The brief says "keep the idle pool's floor
at one warm lease per live writable connection". This implements a floor **at
connect**, not a self-replenishing one: after the 60 s `IDLE_TIMEOUT` reap,
nothing re-warms. A permanent floor would hold an SSH channel and a remote helper
process open for the life of every connection, which is the exact cost
`IDLE_TIMEOUT`'s comment says it was chosen to bound, and decomposition.md §3
says steady-state opens are already fast. The entire measured win (245–325 ms,
`leaseReuse: false` on every first open) is on the first open, and that is what
this takes. **If Mac measurement shows repeat-after-idle opens matter, the
re-warm hook belongs on the reaper** — that is the deferred half.

**Pinned by** 5 tests in the existing `bulk_pool` style. **Mutation-checked, 3/3
caught**: dropping the writable/settled gate (3 tests fail), ignoring an
already-warm connection, establishing a bridge without pooling it.

**Not covered by a test:** the call site in `bridge.rs`. The guard that matters
is `validate()`, tested directly; the wiring is one branch on an already-settled
flag.

**Mac must confirm:** `leaseReuse: true` on the first open of a session, and the
245–325 ms lease segment gone. Also worth watching: that pre-warming does not
perturb connection settling (the "Observed once" failure) — this is the item
that touches that lifecycle.

## 3. Pending-tab skeleton — `164af21`

**What changed.** A third `CombinedTab` variant, `kind: "pending"`, plus
`PendingShellTab`. `useShellNavigation` publishes a placeholder on
`createWindow`/`createSession` before the request is sent, upgrades it with the
real id on the ack, and withdraws it on refusal, throw, or connection
replacement. `App` holds the state; `TabStrip` renders it inert.

**Not optimistic create.** Nothing is reconciled onto the placeholder and it
never carries a temp id. It retires in `combineWorkspaceTabs` once its window
appears in a snapshot, so it and the real tab are never on screen together.

**Why it survives the ack.** The ack is not the snapshot. Withdrawing on the ack
blinks the strip back to empty until the snapshot carrying the new window lands.

**Withdrawal is keyed** to the create that published it: two clicks in quick
succession is ordinary, and the older request answering second is exactly when an
unkeyed withdrawal clears the newer one's placeholder.

**createSession is partial, on purpose.** Its placeholder carries no session id,
so nothing is drawn until the ack names the workspace — a placeholder in the
workspace being navigated *away from* would point at the wrong strip. It
therefore covers the gap *after* the switch (new workspace on screen, snapshot
not yet arrived) but gives **no pre-ack feedback for session creation**. Doing
that properly means a placeholder in the workspace switcher, a different surface
than "tab". Deferred.

**Pinned by** 6 cases in `useShellNavigation.test.tsx` + 1 in `model.test.ts`.
**Mutation-checked, 6/6 caught.** Note: the stale-create test was rewritten
*because of* the mutation check — the first version let the second create
succeed, which republished its own placeholder over the top and passed whether
the guard existed or not.

**Mac must confirm:** the placeholder appears within one frame of the click, and
there is no visible flicker at the ack→snapshot handover (the seam this design
is built around).

## 4. Optimistic window switch — `f5b8544`

**What changed.** In writable mode the switch is painted on click and the
select-window request reconciles behind it. Skipped when there is nowhere to hold
it (no guard supplied, or the window is already showing).

**The snap-back guard is the load-bearing part.** `resolveActiveWindowId` prefers
`windows.find(w => w.active)` — the host's active window — over the local value,
correctly, since another client may have moved it. Every snapshot in the gap
still names the old window, so an unguarded optimistic commit is reverted within
one snapshot. The guard lives in `useAppConnectionController` (where snapshots
decide the active window), mirrors `protectedAppTab`/`observeAuthoritativeWindow`,
and is released only once `hostState.generation` reaches the generation the action
returned — **not on the ack**, which is not the snapshot.

**Failure rolls back actively**, rather than waiting to be corrected: a refused
switch changes nothing on the host, so there may be no further snapshot, and the
shell would sit on a window tmux never selected.

**A bug that survived the first implementation** — worth knowing, because the
same trap is waiting for anyone extending this: committing early moves the same
refs `shellTransitionPlan` reads to decide what to send, so it concluded there was
nowhere to go and **sent nothing at all** — UI on the new window, tmux on the
old. Three pre-existing tests caught it. `requestLocation` now takes an explicit
`origin`, captured before the optimistic commit.

**Deliberately NOT done: de-sequencing `selectSession` → `selectWindow`.** The
brief asks for this; it is not safe as stated, verified on both sides rather than
assumed:

- `apps/host/src/service/tmux_actions.rs:51` rejects any action whose
  `expected_generation` is not an *exact* match, and every successful action
  increments the generation. A concurrently-issued `selectWindow` carries the
  pre-`selectSession` generation and fails with "stale topology: generation
  changed".
- `apps/desktop/src/features/tmux/actionReconciliation.ts:98` excludes a captured
  precondition from stale-topology retry (`&& !capturedPrecondition`), so that
  rejection is **not** retried.

Together: firing them concurrently breaks cross-session navigation rather than
speeding it up. Sending the second unguarded (`expected_generation: 0`) would
disable the staleness check that stops an action landing on a topology that moved
underneath it. And with the switch already painted, nobody waits on either round
trip — the perceived cost the de-sequencing targeted is already gone. If it is
still wanted, it needs a host-side change (a generation *range*, or a compound
select-session-and-window action), not a client-side one.

**Pinned by** 8 cases in `useShellNavigation.test.tsx` + 3 in
`windowSelection.test.ts`; all 48 pre-existing navigation tests stay green.
**Mutation-checked, 7/7 caught.**

**Mac must confirm:** warm switches become 0-RTT-perceived, and — most
importantly — **no snap-back**. Watch a cross-session switch and a switch made
while another client is moving tmux's active window; those are where the guard
is doing real work. This item needs the Step-2 pane-paint spans to show a
before/after at all.

## 5. Large-file open bug — `749e7df`

**The missing termination condition, in one line.** `useOpenFileTab`'s directory
rescan subscription:

```ts
if (generation !== undefined && generation !== shownGeneration()) reloadFromDisk();
```

`shownGeneration()` is undefined until a read *succeeds*. For a file whose read
outlives the gap between rescans, nothing is ever shown — so every rescan
compared a real generation against `undefined`, called it a mismatch, and
reloaded. The reload aborts the in-flight read and starts another, which the next
rescan aborts in turn. The read never advances, so nothing is ever shown, so the
next rescan reloads again. Self-sustaining by construction, once per rescan,
forever — the 171 attempts / 0 successes in the bug report.

**Why only large files.** A small file's read finishes inside one rescan
interval: the generation gets shown, the comparison starts working, the loop
never forms. Nothing about the size threshold is special; it is purely "read
slower than the rescan interval".

**The fix.** "Nothing shown" is not "stale", it is "not yet". The rescan's
opinion is parked rather than acted on, and the read is reconciled against it
once it lands — reusing the `{ kind: "bootstrap" }` machinery that
`reconcileBootstrap` already uses for exactly this question and which *had* the
guard the subscription path lacked. A file that genuinely moved under the read is
still re-read once, just after the read instead of instead of it.

**Pinned by** a new case in `AppTabSurface.test.tsx` driving the real shape: an
open held in flight, three rescans published on top of it, then the open
released. **Written before the fix and confirmed red on the old code**
(`expected true to be false` — the in-flight read had been aborted), green after,
and re-confirmed by mutation.

**No host change, and this is a deliberate answer to the brief's "both sides".**
`file_stream_dispatch` cancels because it was told to, ends the exchange exactly
once, and already has a test for that; the desktop Rust scheduler likewise did
what it was asked. The missing stop condition existed only in the renderer. A
Rust test here would pin nothing that is not already pinned, so none was added.

**Mac must confirm — this is the only item whose *fix* is unproven, not just its
speed.** Everything else lands as "faster, unmeasured"; this one lands as "the
file should now open at all". Run
`run-trial-solo.sh head <dir> p15-large.log tmp` with the 5.2 MB fixture:
`file.open.successes` must go 0 → 1 and `file.open.attempts` collapse from 171 to
roughly 1. If attempts stay high, the loop has a second driver this diagnosis
missed (the `fileChanged` branch two lines below is the candidate — it was left
alone deliberately, since an explicit change event is a real reason to re-read,
and the evidence pointed at rescans).

## Other things found and not fixed

- **The `/tmp` quota**, above. It is an environment fault, not a repo one, but it
  will silently break any agent session on this machine again. `/tmp` accumulates
  scratch across boots and the per-user quota is well under the 14 G mount.
- **`/tmp/ade-measure-build` was a live git worktree** of this repo on
  `perf-measurement` and was deleted with the rest of `/tmp`. No data was lost —
  the branch lives in the main repo's refs and is intact at c65fd63 — but
  `git worktree list` in `~/dev/muxflow` still shows it as `prunable`. Run
  `git worktree prune` there. I did not, because the brief said not to touch the
  main checkout.
- **Two pre-existing clippy warnings** (an elidable lifetime in `tmux-ide-host`,
  a very-complex-type in `tmux-agent-desktop`). Untouched by this work.

## Deliberately not done

- No app launch, no live or perceived-latency measurement — macOS-only harness,
  as instructed.
- Did not touch `~/dev/large-monorepo-fixture`, `~/.local/bin/tmux-ide-host`,
  `~/ade-p15/`, any tmux session, or the `~/dev/muxflow` main checkout.
- The self-replenishing lease floor (item 2), pre-ack feedback for session
  creation (item 3), `selectSession`/`selectWindow` de-sequencing (item 4), and a
  host-side test for item 5, each for the reasons given above.

## Suggested order for the Mac side

1. **Item 5 first.** It is the only item whose *correctness* is unproven rather
   than just its speed, it is the cheapest to check (one solo trial), and a
   still-looping read would invalidate nothing else.
2. **Item 1**, because a startup regression would be the most damaging surprise
   and it is a single before/after on `startup.firstReactCommit`.
3. **Item 2**, watching connection settling as closely as `leaseReuse` — it is
   the item that touches the lifecycle behind decomposition.md's "Observed once".
4. **Items 3 and 4 together**, since both are perceived-latency changes on the
   same surface and both need the Step-2 pane-paint spans to be measurable at
   all. Item 4's risk is snap-back, not speed: if the UI ever reverts a switch,
   revert the commit rather than tuning it.
