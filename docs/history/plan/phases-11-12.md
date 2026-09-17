# Post-V1 Implementation Plan: Phases 11–12

## Context

Phase 10 (macOS readiness and QA) is complete. On 2026-08-13 the user tested the
post-Phase-10 build and delivered the critique that the Phase 11 placeholder in
[`phases-9-11.md`](./phases-9-11.md) was waiting for. This document turns that
critique into two concrete phases and supersedes the Phase 11 placeholder:

1. **Phase 11 — UI redo ("Terminal First").** The user's verdict: the current UI
   has no proportion, a button for everything, and too much chrome instead of
   the terminal being front and center. The approved direction is to copy the
   **cmux** layout model (manaflow-ai/cmux) and render the terminal with the
   **Ghostty** defaults from the user's machine. The approved visual brief is
   the mock at [`phase11-ui-mock.html`](./phase11-ui-mock.html).
2. **Phase 12 — Performance ("Live-session latency").** Everything must be
   snappy: typing, pasting, terminal output, pane/tab switching, the Explorer,
   Git. The target feel is a live tmux session. **The primary use case is the
   Mac desktop connected to tmux on the remote Linux host host over SSH**; local Mac
   is secondary. Installing a dedicated first-party server component on the
   Linux host is explicitly acceptable. Reference products for the feel: existing terminal tool,
   Herdr, cmux.

Numbering follows the reservation in `phases-9-11.md` (11 = UI). **Recommended
execution order is Phase 12 first, then Phase 11** — see
[Sequencing](#sequencing-and-handoff).

Both phases follow the established development cycle: scoped implementation,
deterministic QA, no more than five independent high-rigor reviews, fixes for
all accepted findings, append-only `implementation.md` evidence, and cleanup.
Evidence lives under `docs/history/qa/ui-polish/` and `tests/performance/runtime/`.

## Baseline and invariants

- The Phase 10 release candidate (Linux + internal Apple-Silicon macOS) is the
  behavioral baseline.
- tmux remains authoritative for persistent sessions, windows, panes, and
  processes. A tmux session is a workspace, a tmux window is a terminal tab, a
  tmux pane is a split. Phase 12 may change **how bytes and state travel**, not
  who owns the topology.
- Codex/Claude lifecycle, notification routing, Explorer, editor, Markdown,
  Git, upload/download, reconnect, and external-client tmux synchronization
  must not regress.
- xterm.js/WebGL remains the terminal renderer. Do not adopt libghostty
  embedding (its embedding API is explicitly unstable and incompatible with a
  WebView) and do not swap the parser for ghostty-web/restty this cycle (the
  xterm.js maintainer's own libghostty prototype measured parser performance
  "similar to the current parser" — xtermjs/xterm.js#5686).
- Protocol changes require version negotiation and the existing explicit
  helper-upgrade flow; old helpers enter the read-only/incompatible path.
- Verification uses disk-backed `tmp/work`, bounded caches, compact evidence,
  and cleanup traps. Exact 5 GiB transfer lanes run only if transfer code
  changes.

---

## Phase 11 — UI redo ("Terminal First") — COMPLETE 2026-08-14

Implemented and verified; evidence in `docs/history/qa/ui-polish/`, ledger entry in
`implementation.md`. One acceptance number was not met and was not worked
around: the terminal is 74.4% of a 1280x800 window by area with the sidebar
open (91.4% with ⌘B), not the ≥85% asked for below. That figure is unreachable
with the 240px sidebar this same table mandates — it would need a 90px sidebar
— and the mock measures 77.7% itself, its "~86%" caption being a width share
rather than an area share. See `docs/history/qa/ui-polish/gates.md`.

**The user reviewed the running UI on 2026-08-14 and accepted 74.4% as it
stands, with ⌘B reaching 91.4%.** The ≥85% gate below is therefore retired, not
outstanding; the layout is unchanged and no sidebar width was traded for it.

### Approved brief

The mock [`phase11-ui-mock.html`](./phase11-ui-mock.html) is the acceptance
reference: four states (default, diff-open, agents, command palette), a
before/after table, and the design tokens. The governing rules, in priority
order:

1. **The terminal is the product.** ≥85% of the window at rest. Editors, git
   diffs, and markdown open as **document tabs in the same strip as terminal
   tabs** (the VS Code model, per user feedback on the mock) — a takeover is
   fine because the terminal is one keystroke (⌃1) away, never buried behind
   chrome.
2. **Copy cmux's layout system**: 28px bars everywhere; one 240px left sidebar
   of rich workspace rows; slim window-tab strip; full-bleed tiled panes with
   hairline dividers; optional right tool panel; **no status bar**; one accent
   color (`#0091FF`) for selection, attention, badges, and links.
3. **Copy Ghostty's terminal look, verbatim from this machine**: JetBrains Mono
   13 (bundled), background `#282C34`, white foreground and block cursor,
   inverted selection, the "Ghostty Default Style Dark" ANSI palette.
4. **Delete chrome instead of rearranging it.** ~20 at-rest controls become 7.
   Removed buttons become palette commands, context-menu items, and shortcuts.

### Current-state evidence (why each change exists)

Measured against the Phase 10 build (screenshots in
`tests/acceptance/macos/evidence/exploratory/`):

- 680px of fixed horizontal chrome (176px rail + 246px sidebar + 258px agents
  panel) regardless of window size (`apps/desktop/src/styles.css:20`); the
  terminal gets ~41% of window area, ~39 columns per split pane, and 0% when a
  diff or file opens.
- 89 `<button>` JSX sites plus 7 disclosure menus; ~20 visible controls at
  rest, 2 of which concern the terminal.
- Connection state reported in three places at once (Live strip, Active-pane
  block, floating pane badge that overlaps output).
- No design tokens: ~150 hard-coded hex literals in one 377-line stylesheet
  plus a second divergent palette in `TerminalRenderer.ts:227-235`. Inter is
  referenced but never bundled (`styles.css:2`). Icons are text glyphs. Type
  runs 9–17px with no scale. Context menus are ~195px wide with 39px rows.
  Shortcuts render as `Meta+Shift+P` (finding M10-E052).

### Design system (authoritative token values)

| Token | Value | Source |
| --- | --- | --- |
| Chrome background | `#101114` (raised `#16181C`, hairline `#1C1E22`) | cmux-style near-black, darker than terminal |
| Terminal background | `#282C34`, fg `#FFFFFF`, cursor white block, selection inverted | Ghostty default (theme file verbatim, ANSI 0–15 included) |
| Accent (the only one) | `#0091FF` | cmux `cmuxAccentNSColor` |
| Destructive | one red treatment (`#CC6566` family), used identically everywhere | replaces today's four reds |
| Chrome type | system font (SF): row title 12.5 semibold, secondary 10.5, metadata 10 mono, section labels 9.5 caps | cmux sidebar typography |
| Terminal type | JetBrains Mono 13 / line-height 1.42, **bundled via `@font-face`** | Ghostty default font |
| Bar heights | 28px, all bars (titlebar, tab strip, panel tabs) | cmux `sharedChromeBarHeight` |
| Sidebar | 240px default/min, drag-resizable, capped at ⅓ window, ⌘B → 0 | cmux |
| Right panel | 300px default, ⌥⌘B, hidden by default | cmux right sidebar |
| Focus/attention | active pane ring 2px accent; attention ring 2.5px, radius 6, inset 2 | cmux `PaneChromeSettings` / `PanelOverlayRingMetrics` |
| Icons | one SVG glyph set, SF-Symbols-weight strokes; no text-glyph icons | — |
| Menus/palette | context menus 13px text / 24px rows; palette 560px wide / 28px rows | desktop scale |

All tokens land as CSS custom properties in one file before any screen is
edited (per the planning gate in `phases-9-11.md`). The terminal theme and the
chrome theme derive from the same token file.

### 11.1 Foundations

1. Introduce the token file (colors, type scale, spacing, bar metrics, rings)
   and migrate `styles.css` and `TerminalRenderer.ts` theme to consume it.
2. Bundle JetBrains Mono (regular/bold/italic subsets) and set the terminal
   font stack to it; remove the dead Inter reference; chrome uses the system
   font stack.
3. Add the SVG icon set (sidebar toggle, panel toggle, bell, plus, split-h,
   split-v, close, chevrons, search, branch, file states). Delete text-glyph
   icons.
4. Replace shortcut rendering with platform glyphs (`⌘⇧P`) — closes M10-E052.

### 11.2 Shell layout

1. Rebuild the shell as: 28px titlebar (traffic lights, sidebar toggle,
   back/forward focus history, workspace title + branch, bell + unread badge,
   new-workspace, panel toggle) — the window uses a hidden/overlay titlebar.
2. Left sidebar (240px), **two stacked sections with a draggable divider —
   the Herdr model** (user-approved reference for the agents UI):
   - **Workspaces (top):** tmux sessions as rich rows — name (12.5
     semibold), last agent event snippet (10.5, 2-line clamp),
     `branch* · cwd` metadata line (10 mono), unread badge, working spinner.
     Active row = solid accent fill, white text. A workspace row carries the
     aggregate state of its loudest agent.
   - **Agents (bottom):** a **flat list across all workspaces** (Herdr
     shipped and then deliberately removed per-workspace scoping — do not
     build a scope switcher). Two-line rows: state dot + workspace + tab
     index, then `agent · activity`. The section header carries a one-word
     clickable sort toggle, persisted: `grouped` (workspace order, default)
     ⇄ `priority` (attention desc, then recency). Attention ranking copies
     Herdr: **blocked > done-unread > working > idle** — a finished agent
     stays "done" (teal) until its pane is focused, making the list an
     inbox. State encoding: yellow = working, red = blocked, teal =
     done-unread, hollow = idle; color-only dots by default with a
     shape-glyph option for accessibility. Clicking a row selects the
     workspace, switches to the agent's tmux window, and focuses its exact
     pane via the existing exact-routing path; ⌘⇧U jumps to the top of the
     priority order.
   Bottom host row: `● host · transport · latency` — the **only** resting
   connection indicator.
3. Window-tab strip (28px): one strip, two kinds of tabs — **terminal tabs**
   (tmux windows: index + name + activity dot) and **document tabs** (files,
   git diffs, markdown, with a type glyph). Split/new controls at the right
   edge. Tab switching must not reflow other tabs (today's active tab injects
   four inline buttons).
4. Panes: full-bleed terminals, 1px hairline dividers, tmux-authoritative
   layout, 2px accent focus ring, 2.5px attention ring. Delete the floating
   `%N · cmd` pane badge.
5. Delete: workspace rail as a separate column, the Live banner (replaced by a
   thin amber strip shown **only** while disconnected/reconnecting), the
   Active-pane block, the docked Sounds mixer (moves to Settings), the
   permanent commit form (appears only when staged files exist).

### 11.3 Documents as tabs; the right panel

1. Editor, diff, and Markdown surfaces open as **document tabs in the main
   strip**, full-surface Monaco, coexisting with terminal tabs (VS Code
   model). Clicking a file in the Explorer or a row in Git opens (or focuses)
   its document tab; ⌃1–9 and ⌘⇧[/] move across both kinds of tabs.
2. The right panel (⌥⌘B) hosts **Files ⇄ Git** as one 300px surface with 28px
   segment tabs, closed by default. Explorer rows and Git file rows are mono
   11px with single-letter states; per-row actions move to context menus and
   the palette. Agents live in the sidebar's agents section (11.2), not in
   this panel.
3. Empty states are one quiet line, not illustrated cards; the panel reserves
   no space when hidden.

### 11.4 Interaction model

1. Adopt the cmux keymap as default: ⌘N new workspace, ⌘P fuzzy workspace
   switcher, ⌘1–9 workspaces, ⌃1–9 tabs, ⌘B/⌥⌘B rails, ⌘D/⌘⇧D splits,
   ⌘⇧↩ zoom, ⌘K palette, ⌘⇧U jump to latest unread. Keep the existing
   shortcut editor; keep all commands in the single registry.
2. Palette rework: 560px, 28px rows, inline group labels (no repeating
   category column), no empty shortcut lozenges, fuzzy match.
3. Context menus at desktop scale; destructive items use the single red
   treatment; confirmations keep the existing destructive-action contract.
4. Rejection/error banners: scoped to the surface that raised them, dismissed
   with it, human-readable summary with details behind a disclosure — closes
   M10-E059.

### 11.5 States, accessibility, and platform feel

1. Deliberately test empty and plural states as named cases (one workspace vs
   many; zero agents vs several; per the Phase 10 lesson in
   `implementation.md`).
2. Preserve: VoiceOver/AX labels and roles, keyboard operability of every
   surface, `forced-colors`, `prefers-reduced-motion`, IME composition,
   narrow-width collapse (sidebar overlays below 880px), Retina and
   multi-display behavior.
3. Linux parity: same layout and tokens; verify fonts and window chrome on the
   Linux package.

### Phase 11 verification and acceptance

1. Frontend typecheck/tests/build; Rust untouched surfaces still green;
   bounded Linux regression lane.
2. Screenshot acceptance against the mock for: default state, diff-open state
   (document tab), agents-in-sidebar state, palette state, disconnected
   strip, empty and plural sidebar states, narrow width, full screen — on
   macOS and Linux.
3. Quantitative gates measured on the packaged app:
   - fixed chrome at rest ≤ 240px + 2×28px bars; terminal ≥ 85% of window
     area at rest in a 1280×800 window — **user-accepted at 74.4% on
     2026-08-14** (91.4% with ⌘B); see the note above the brief;
   - terminal and document tabs coexist in one strip; ⌃1 returns to a
     terminal from any document tab;
   - clicking an agents-section row focuses the agent's exact window/pane
     (verified against tmux state, not just UI);
   - visible interactive controls at rest ≤ 8;
   - exactly one resting connection indicator;
   - all shortcut chips render platform glyphs.
4. Phase 12 performance budgets (below) re-run and green after the UI rebuild
   if Phase 12 shipped first; otherwise current Phase 8/10 latency evidence
   must not regress.
5. Five-review cycle, findings dispositioned, `implementation.md` entry,
   evidence under `docs/history/qa/ui-polish/`.

### Phase 11 non-goals

No workflow/product changes beyond the brief: no new Git network features, no
pane-title rename UI, no light theme (tokens make one possible later), no
renderer swap, no protocol changes. If a visual change requires a protocol or
host change, it needs explicit scope escalation.

---

## Phase 12 — Performance ("Live-session latency")

### Goal

Interactive use of the Mac desktop against tmux on a remote Linux host over
SSH (50–150 ms RTT) must feel like a native terminal with a live tmux
session; local Mac must feel indistinguishable from Ghostty+tmux. The phase is
budget-driven: every budget gets a measurement harness, and the phase closes
on measured numbers, not RPC-ack timings (the Phase 10 ledger's own gap).

### Budgets (acceptance targets)

| Metric | Local Mac | Remote @ 100 ms RTT | Today (measured) |
| --- | --- | --- | --- |
| Keystroke → glyph echo, p95 | ≤ 35 ms | ≤ RTT + 35 ms (≤ 50 ms perceived with prediction, if 12.7 ships) | 21.8 ms/key *fork cost alone* local; 2816 ms / 129 keys in the Phase 2 driver |
| Sustained output without input degradation | ≥ 10 MB/s, no full resync | link-bound, no full resync | 128-event queue overflow → connection-wide resync |
| Tab/pane switch (visible + interactive) | ≤ 100 ms | ≤ 150 ms, **zero awaited SSH round-trips** | unmount + 10k-line serialize + up-to-16 MB JSON upload + awaited round-trip |
| New workspace / new tab (action → pane interactive) | ≤ 300 ms | ≤ 500 ms | user-reported **multiple seconds** on local Mac; cause under spike investigation (Stage 12.0, spike 2) |
| Explorer: expand 4,096-entry directory | ≤ 150 ms render, 60 fps scroll | + one listing round-trip | renders 4,096 DOM rows, no virtualization |
| External file change visible in tree | ≤ 300 ms, delta-applied | ≤ 300 ms + RTT | full parent re-list per changed path |
| Idle steady-state traffic | 0 periodic round-trips | 0 periodic round-trips | 350 ms active-root poll (~12 tmux forks/s remote) + 2 s reconcile |
| Resize settle | ≤ 100 ms | ≤ 150 ms | 60 ms debounce + input-barrier flush |
| Reconnect after 2 s network stall | no disconnect | no disconnect | `ServerAliveInterval=1,CountMax=2` kills the session |

The governing comparison, per the user's own baseline ("plain ssh+tmux to
remote Linux host feels good; the app feels slow even locally"): the harness measures
keystroke echo side by side against a raw `ssh <host>` + tmux session on the
same link, and the app's added overhead beyond that baseline must be
**≤ 10 ms p95** — locally and remotely. Parity with raw ssh+tmux is the pass
bar; prediction (12.7) is the only thing that can beat it and stays gated.

### Evidence-based bottleneck ledger

Full audit with hop maps and constants: `tests/performance/runtime/bottleneck-audit.md`
(committed from this planning round), **amended by the Stage 12.0 spike**
(`tests/performance/runtime/spike-report-2026-08-13.md`): item 2 below is re-scoped to
the desktop side, coalescing in item 3 is demoted, and batched single-fork
discovery + the reconciliation-budget cut are added as P0s — see 12.1.
Ranked P0s from the audit:

1. **Input forks tmux 3× per keystroke** and blocks a Tauri command thread up
   to 5 s on the round trip (`apps/host/src/service/terminal/input.rs:81-150`,
   `apps/desktop/src-tauri/src/connection.rs:150-186`). Contradicts
   `technical-plan.md:232` ("send-keys -H … without waiting per keystroke").
   The Phase 2 driver bound was widened (2 s → 4 s) to accommodate this
   (`tests/acceptance/macos/findings.md:53`) — that widening is reverted by this phase.
2. **The host connection reader awaits terminal work inline**
   (`apps/host/src/service.rs:254-289`): a split/resize runs a ~6-fork tmux
   discovery while keystrokes queue behind it.
3. **Zero output coalescing**: one `%output` record = one flushed protobuf
   frame = one IPC message (`stream.rs:203-243`, `service.rs:123-133`);
   overflow of the 128-deep queue escalates to a connection-wide
   `ResyncRequired` and full reseed.
4. **Tab switch destroys terminals** and ships a JSON `Array.from()` number
   array of up to 4 MiB of snapshot bytes across the WebView boundary
   (`features/shell/model.ts:388-396`, `terminal/api.ts:298-310`).
5. **350 ms active-root poll** with double `discover_authoritative` per call;
   **new SSH connection per file open/save** (`transport.rs:147-175`);
   **`git status --ignored=matching`** enumerating `node_modules` for entries
   the frontend filters out; unfiltered recursive git watcher; SSH keepalive
   `1s/2` tears the session down on any 2 s stall; React layer has zero
   memoization, per-chunk viewport `setState`, and no list virtualization;
   dev builds run fully unoptimized (no `[profile.dev]` overrides).

### Architecture decision: what happens to tmux

Options considered against the references (existing terminal tool: own PTY daemon + Electron;
Herdr: own Rust daemon replacing tmux, admits frame-round-trip echo latency;
cmux: native libghostty + first-party `cmuxd-remote` over SSH stdio, hit
tmux's per-keystroke repaint amplification as its dominant remote cost —
manaflow-ai/cmux#4681):

- **A. Keep tmux authoritative, fix the data path around it** (this phase's
  Stage 12.1–12.6). tmux stays the topology and persistence authority;
  `tmux-ide-host` — our existing first-party server on the Linux host — gets
  smarter: in-band input, coalesced push, delta sync, server-owned pane state.
  No new third-party requirement.
- **B. Server-side terminal state sync + predictive echo** (Stage 12.7,
  gated): run a VT emulator in the host daemon (`wezterm-term`, or
  `libghostty-vt` when its API stabilizes) so the wire carries cell diffs
  paced at Mosh's ½·SRTT (clamped 20–250 ms) instead of raw byte bursts —
  this neutralizes tmux's per-keystroke repaint amplification — plus
  Mosh-style speculative local echo in the frontend (VS Code and WezTerm both
  ship this pattern in production).
- **C. Replace tmux** (zellij / shpool / own portable-pty daemon). Rejected
  this cycle: it breaks the product's core invariant (mirror the user's
  existing tmux server, external clients stay in sync) and resets Phase 0–10
  QA. Revisit only if A+B miss budgets. If reconnect-survivable transport is
  wanted later, Eternal Terminal is the option compatible with control mode
  (Mosh is not — control mode needs a reliable ordered stream).

**Decision: A now, B gated.** Stage 12.7 begins only if the Stage 12.1–12.6
measurements leave the remote echo budget unmet, or the user explicitly asks
for prediction regardless.

### 12.1 Input and action dispatch (P0 — spike-validated 2026-08-13)

Spike evidence: `tests/performance/runtime/spike-report-2026-08-13.md`. A tmux client
fork costs ~6 ms on macOS; fork count explains nearly every host-side
latency. Keystroke: 17.5 ms → **0.05–0.14 ms** (−99.7%) with in-band input;
CreateWindow/CreateSession: 110–127 ms → **~31 ms** (−73%) with batched
discovery. Full Phase 2 matrix green under both flags.

1. Replace the 3-fork `load-buffer`/`paste-buffer` path with `send-keys -H`
   written to the already-open tmux `-C` control client's stdin, for payloads
   under ~1 KiB; keep the buffer path only for large pastes (preserving
   bracketed-paste-once and no-implicit-Enter contracts). Two spike-found
   obligations: (a) make explicit — comment + test — the invariant that the
   `__ADE_CAPTURE__` marker and `capture-pane` are written under one stdin
   lock hold, since interleaved input writes would corrupt seed correlation;
   (b) design the replacement for the lost synchronous validation and
   commit-point error contract (vanished pane surfaces as async `%error` →
   resnapshot; lazy error via the event stream).
2. Make `TerminalInput` fire-and-forget on the desktop side: no completion
   await; errors surface via the event stream. Remove the 5 s blocking
   `recv_timeout` from the keystroke path.
3. **Batch tmux discovery into a single fork** — one tmux invocation
   chaining `display-message ; list-sessions ; list-windows ; list-panes` —
   and **delete the redundant re-discovery inside `tmux_actions::execute`**
   (the dispatcher's snapshot is fresh; one client invocation is bound to
   one server, which also removes the race `discover_consistent` guards).
   ~90 lines, −73% action latency, −74% `controlLatencyMs` measured.
4. **Make every host-touching Tauri command async** (they default to
   blocking `ExecutionContext::Blocking` on the macOS main thread today:
   `tmux_action`, `set_terminal_visibility`, `request_terminal_seed`,
   `file_request`, `git_request` — 5-minute timeout! — `agent_request`,
   `save_app_state`). This replaces the audit's "detach host terminal ops"
   item, which the spike measured as negligible host-side; the frozen-UI
   cost is desktop-side.
5. Pass input and snapshot bytes as raw `Uint8Array` payloads (Tauri v2 raw
   serialization), eliminating every `Array.from()` JSON number array — the
   spike attributes **~1–2 s per window switch** to the JSON path at the
   4 MiB cap.
6. Cut the action-reconciliation budget by an order of magnitude
   (`ACTION_RECONCILE_TIMEOUT_MS` 2000 → ~250 ms, retaining retries): with
   batched discovery the host answers in ~31 ms, and the 2 s × 2-retry loop
   is the amplifier that turned `stale_topology` (hit in 2/3 baseline spike
   runs) into the user's multi-second creates.

### 12.2 Output path (P1 — coalescing demoted by spike measurement)

1. Merge same-pane `%output` records at read granularity (≤ 32 KiB, no
   timer — zero added latency). The spike measured only a **16%** frame
   reduction because tmux already emits large records, so this is
   opportunistic, not a P0; do not build a timer-based window on the reader.
2. Replace connection-wide `ResyncRequired` on event-queue overflow with
   per-pane drop-and-reseed; raise the 128-event queue; keep topology events
   on a lane that cannot be starved by output.
3. End-to-end ack watermark: the frontend acks parsed bytes (xterm `write`
   callback, pending-callback-count variant, HIGH ≤ 500 KB per xterm.js
   guidance); the host pauses per-pane reads above the high mark — replacing
   cliff-edge 8 MiB drop/reseed fallbacks with smooth backpressure.
4. Revisit `pause-after` handling: on `%pause`, resume the stream without an
   unconditional full `capture-pane` recapture.

### 12.3 Kill polling; push instead (P1)

1. Delete the 350 ms active-root poll; the host pushes `ActiveRoot` on tmux
   focus/window-pane-change notifications, with the existing cache as floor.
2. Explorer applies the host's per-path deltas instead of re-listing the
   parent directory per change; listings become versioned so deltas compose.
3. Git: drop `--ignored=matching`; add exclusion filters to the recursive
   watcher (`.git` internals churn, `node_modules`, `target`, build dirs
   honoring `.gitignore`); add an index-mtime short-circuit and a minimum
   inter-refresh interval before forking; batch the status+diff invocations
   per refresh.
4. Reduce the 2 s safety reconcile to a slow heartbeat (≥ 30 s) now that
   notification-driven reconciliation is proven.

### 12.4 Pane lifecycle (P0)

1. Keep hidden panes mounted (`display:none`/offscreen) within a bounded live
   set (active window's panes + most-recent N); WebGL resource release stays
   for panes beyond the bound.
2. Move pane-snapshot ownership to the host: hide/reveal becomes a pane-id
   message, never a 4 MiB snapshot upload; the host already holds the
   authoritative screen via tmux.
3. Make the hide handoff non-awaited; tab switch renders immediately.

### 12.5 Remote transport (P1)

1. `ServerAliveInterval=15, ServerAliveCountMax=3`; `Compression=yes`
   (terminal output compresses 5–10×); `IPQoS=lowdelay` for the control lane.
2. Keep a second persistent SSH master (`ControlPersist`) for bulk/editor
   I/O; route reads/writes ≤ 1 MiB over the control connection — a 3 KB file
   open stops paying a full TCP+KEX+auth handshake.
3. Adopt cmux-style reconnect hygiene: exponential backoff to a 60 s cap with
   jitter; smallest-screen-wins resize across multiple attachments is already
   tmux semantics — verify it holds.

### 12.6 Frontend render path (P2)

1. Gate `#emitViewport` on actual state change (today: one React `setState`
   per output chunk per pane); make `screenReaderMode` opt-in (AX toggle);
   drop `smoothScrollDuration` to 0 during floods. Add an idle fast path to
   the write scheduler: when the pending queue is empty, write to xterm
   immediately instead of waiting for the next rAF tick (which quantizes a
   single echoed keystroke by up to 16 ms); rAF pacing engages only under
   load. The keystroke budget assumes this: echo must not pay a frame of
   queueing latency at idle.
2. Decompose `App.tsx` state so status/topology churn cannot re-render the
   terminal surface, sidebars, and dialogs; add `React.memo` at the feature
   boundaries. (If Phase 11 ships second, this lands as part of its rebuild;
   the budget still gates here.)
3. Virtualize the Explorer tree (fixed 24px rows, overscan) and the Git file
   list; stabilize `listings` identity so memoization holds.
3b. Spike-found stability fixes: stop keying `fileScope` on `paneId`
   (`files/api.ts:332-334`) so a create/tab-switch doesn't rebuild the
   entire explorer scope (root re-resolve + 4096-entry re-list + re-watch);
   stabilize `paneTransferScope` identity (`TerminalPane.tsx:127-131`) so
   drag-drop listeners stop re-registering — 4 listeners + 4 unlistens per
   pane per render today; stop re-bootstrapping the git watch and refetching
   the full agent snapshot on every topology generation bump.
4. `[profile.dev] opt-level = 1` + `[profile.dev.package."*"] opt-level = 3`
   so development builds stop running the parser/protocol stack at opt-level
   0.

### 12.7 Predictive local echo (gated Stage B)

Mosh's algorithm (USENIX ATC '12: 70% of keystrokes echoed instantly, 0.9%
mispredictions, all corrected within one RTT), as productionized by VS Code
(`localEchoLatencyThreshold`, dimmed unconfirmed glyphs, per-program
auto-disable) and WezTerm (`local_echo_threshold_ms`):

1. Client-side prediction overlay in the renderer wrapper: printable keys echo
   at the cursor immediately, dimmed until confirmed; epochs invalidate on
   control chars/newline/arrows; per-row confirmation before display;
   underline predictions outstanding > 100 ms.
2. Engage only above a measured ~30 ms RTT; per-program exclusion (vim/TUI
   alternate-screen detection); instant rollback on misprediction.
3. If Stage B extends to server-side state sync (cell diffs via a host-side VT
   emulator), adopt Mosh's pacing constants: frame interval ½·SRTT clamped
   [20, 250] ms, ~15 ms sender coalescing, delayed acks ≤ 100 ms.

### 12.8 Measurement harness (ships first, in Stage 12.0)

1. Keystroke-to-glyph latency probe: injected key events timestamped against
   renderer paint (rAF-after-write), p50/p95, local and shaped 100 ms SSH;
   replaces RPC-ack proxies. Wire into `tests/performance/runtime/` with budgets as
   pass/fail bounds.
2. Output throughput + frame-drop probe (`yes`/`cat` floods, agent-style burst
   fixtures), pane-switch timing, explorer expand/scroll timing, idle-traffic
   counter (asserts zero periodic round-trips), reconnect-stall probe.
3. Reuse existing instrumentation (`max-control-latency-micros`, transfer
   `maxControlLatencyMs`, event-queue overflow counters) and record before/
   after per stage. Revert the widened Phase 2 (4 s) and Phase 8 (macOS)
   bounds once fixes land.

### Phase 12 execution order and gates

- **Stage 12.0 — spikes first, then harness.**
  **Spikes: COMPLETE 2026-08-13** — report at
  `tests/performance/runtime/spike-report-2026-08-13.md`, patch at
  `tests/performance/runtime/spike/spike.patch`. Results: in-band input −99.7%
  keystroke cost (matrix green); batched single-fork discovery −73% action
  latency; the user's multi-second creates are frontend-side (blocking
  main-thread Tauri commands + 1–2 s JSON snapshot serialization + the
  2 s × 2 reconciliation ceiling); coalescing demoted (−16% only). The P0
  list in 12.1 was amended accordingly; two audit suspicions
  (`same_action_topology` serde, reconcile-attach forks) were disproven and
  removed from scope.
  **Then the measurement harness** (12.8) + baseline capture (local +
  B-Docker 100 ms + a read-only remote Linux host latency sample), with keystroke,
  create-action, and window-switch probes shaped by the spike findings.
  Gate: budgets table populated with "today" numbers before Stage 12.1
  commits to implementation order.
- **Stage 12.1–12.2** (input + output P0s) → re-measure. Gate: keystroke and
  flood budgets met or materially improved on B-Docker.
- **Stage 12.3–12.6** in bounded batches, re-measuring per batch.
- **Stage 12.7** only if gated in (see decision above).
- Five-review cycle per repo convention, with special attention to: input
  ordering and interleaving guarantees (send-keys vs paste-buffer, bracketed
  paste), backpressure correctness (no deadlocks between ack watermark and
  pause/continue), reconnect/reseed races, protocol compatibility and helper
  upgrade, and cross-platform (Linux regression lane) parity.

### 12.9 Hand-test remediation — agent-pane realtime correctness

Added 2026-08-13 after the user's first hands-on test; **root-caused the
same day** (evidence and mechanism detail: `tests/performance/runtime/findings.md`).
This section is the execution plan for the fixes. The implementer must not
re-derive causes from symptoms — the causes below are proven or
code-confirmed, each with the check that validates it. Work the items in
order; every item names its acceptance gate. Gated ahead of Phase 11.

**Already-built instrumentation you must use, not rebuild:**
- `tests/performance/runtime/run-pause-probe.sh` + driver scenario `pause-probe`
  (`tests/performance/runtime/perf-driver/src/main.rs`): deterministic P12-U001 repro.
  SIGSTOPs the host daemon 8 s while one pane floods and one ticks at 1 Hz,
  then requires both a visible and a hidden pane to resume and match
  `capture-pane` ground truth. **Committed red. Item 2 must turn it green
  without changing the probe's assertions.**
- `tests/performance/runtime/spike/pause-continue-quote.patch`: the proven one-line
  U001 fix, kept as evidence. Re-apply it properly (with tests), don't
  reinvent it.

1. **U001 fix — quote the continue argument.** `apps/host/src/service/
   terminal.rs` `write_capture_request_resuming` writes
   `refresh-client -A %N:continue`; tmux's lexer rejects any unquoted word
   starting with `%` unless it is all digits (`%N:continue` contains `:` →
   `parse error: syntax error`, pane stays paused forever). Change to
   `refresh-client -A '%N:continue'`. Add a unit test asserting the quoted
   form (byte-exact, like the existing marker tests) with a comment naming
   the lexer rule, and grep the host for any other command that embeds a
   `%`-prefixed word with a non-digit suffix (today there is exactly one
   site). Gate: `run-pause-probe.sh` prints PASS.
2. **U001 hardening — %error fidelity and scoping.** Same repro showed two
   compounding bugs in `stream.rs`:
   a. `ControlRecord::Error` reports only the `%error` header numbers and
      throws away the error text lines inside the block. Carry the block's
      output lines into the emitted detail so the next tmux rejection is
      readable in one log line.
   b. A failed **continue/capture** command for pane %N must emit a
      **pane-scoped** resnapshot, not `scope: "terminal"`. The writer knows
      which pane each continue+capture group targets; thread that through
      so an `%error` on it is attributed to the pane. (This unscoped error
      is what turned each rejected continue into a connection-wide resync —
      the P12-Q005 signature.)
   c. tmux semantics to encode as tests (established by probe, see
      findings): output during pause is dropped (reseed after continue is
      mandatory — the existing continue→capture order is correct); continue
      on a non-paused pane is a silent no-op (safe to blanket-issue);
      `%continue` is not an ack — the driver watchdog (pane advances while
      ground truth advances) is the only real ack; notifications may arrive
      *inside* `%begin/%end` blocks — the parser/stream reader must
      recognize `%pause`/`%continue`/topology notifications there instead
      of swallowing them as command output (also closes a capture-content
      pollution corruption vector).
   Gate: new host unit tests + `run-pause-probe.sh` PASS with
   `connectionWideResyncEvents == 0`.
3. **U002 fix — desktop echo/repaint leg.** Mechanisms are code-confirmed;
   measure magnitudes first, then fix in this order, re-measuring after
   each (keystroke wire-echo from the driver is the metric; add an
   agent-shaped lane: a fixture pane that repaints a 4 KB cursor-addressed
   frame wrapped in `ESC[?2026h/l` on every keystroke echo and at 1 Hz):
   a. **Write scheduler throughput.** `TerminalRenderer.ts` `#flush`
      drains exactly one queued event per rAF. Change it to drain the
      queue up to `maxBytesPerFrame` bytes per frame (loop across entries,
      preserving order and the per-entry `onRendered` callbacks).
   b. **`screenReaderMode: true`** (`TerminalRenderer.ts:251`) — turn it
      off by default. It costs a string alloc + emitter dispatch per
      printed codepoint plus a DOM row-mirror rewrite per render, and its
      overlay causes the stale-selection artifact (U003.4). If
      accessibility matters, expose it as a setting; do not leave it on
      unconditionally.
      **Carried forward 2026-08-14:** it was turned off, and no setting was
      added — this app has no preferences surface to hang one on yet. That
      leaves terminal *content* unreadable to a screen reader (the pane's AX
      label, role and keyboard operability are unaffected). Phase 11 owns the
      settings surface; the toggle is a tracked follow-up there, recorded in
      `tests/performance/runtime/findings.md` under P12-U002.
   c. Re-measure. Budget: agent-lane echo p95 within 2× of plain-lane echo
      p95, both within the Phase 12 keystroke budget; local plain-lane
      numbers must not regress vs `tests/performance/runtime` finals.
      **Amended 2026-08-14 during execution, with the measurements in hand.**
      The ratio is reported as INFO, not asserted. Two runs of the same lane:
      plain p95 0.773 ms / agent p95 2.909 ms (3.76×), and plain p95 1.192 ms
      / agent p95 1.905 ms (1.60×). Both lanes are ~15-45× inside the 35 ms
      keystroke budget in both runs, and the ratio between two sub-millisecond
      p95s of *different payloads* — one echoed byte against a 4 KiB repaint —
      moves by more than 2× on noise alone. Gating on it would make a green
      lane a coin flip. The lane therefore gates on the absolute budget, zero
      connection-wide resyncs and no sequence gaps. The renderer-side
      magnitude U002 is actually about is gated separately and
      deterministically by the agent-repaint frame-cost test in the desktop
      suite (23 frames → 1 frame for a 24-record 4 KiB repaint).
   Host note: `observe_screen` was audited — 4 Hz self-throttle, 8 KiB
   tail, cheap; moving it off the reader thread is OPTIONAL hardening, not
   part of this fix. Do not spend budget there unless the agent-lane
   measurement, with a bound agent, still shows reader-thread stalls.
4. **U003 fix — corruption mechanisms, in order of evidence strength.**
   Build the parity lane first: extend the pause-probe fixture with an
   agent-TUI pane (alternate screen + cursor addressing + 2026 brackets +
   1 Hz repaint) and a driver check that renders delivered bytes through a
   vt parser (add `vt100` crate to the driver) and diffs the full screen
   against `capture-pane -e` at quiesce points across seed, hide/reveal,
   pause/continue, recovery, reconnect. Byte-exact parity is the gate for
   every sub-item.
   a. **Geometry divergence.** `App.tsx:351` sends resize to tmux only for
      the active pane while `FitAddon.fit()` resizes every mounted pane
      locally. Fix: reconcile every mounted pane's xterm cols/rows with the
      topology snapshot's `pane.width/height` (tmux is authoritative;
      constrain xterm to tmux's grid rather than the CSS box), and log a
      diagnostic if they ever diverge. Verify first (cheap): log both values
      on a split layout — the agent found rounding divergence is the norm.
   b. **2026 watchdog tearing.** xterm 6 force-clears synchronized output
      1000 ms after the first held refresh (RenderService watchdog) and
      repaints a half-applied frame. 3a (scheduler drains whole queue per
      frame) removes the delay source; after it, assert in the agent lane
      that `terminal.modes.synchronizedOutputMode` never stays true >100 ms
      under continuous repaints.
   c. **Restore-over-newer-output.** In the renderer/hub pair: make
      `#lastAppliedGeneration` monotonic (ignore regressions); gate
      `restore()` on `tailThroughGeneration >=` the last applied
      generation, otherwise request a fresh seed instead of restoring; fix
      the hide/reveal checkpoint divergence (hide uses the renderer's
      counter, reveal uses the hub's; `markRendered` is silenced during the
      hide drain by the early `rendererActive = false`) so both ends report
      the same cutoff; make `TerminalEventHub.publish`'s
      `generation <= last` drop of a **seed** impossible (a seed must
      always apply or trigger an explicit reseed request); surface (don't
      silently ignore) `restore()` no-ops while `#overflowed` is latched,
      and make every `enqueue() === false` after overflow request a seed
      rather than dropping bytes forever.
   d. **Serialize addon parity.** `@xterm/addon-serialize@0.13.0` declares
      peer xterm ^5 against installed 6.0.0 and reads private internals.
      Check upstream for the 6.x-paired release and upgrade; then diff a
      serialized+restored styled screen against the original in the parity
      lane (bold/dim/selection attributes included).
   Gate: parity lane byte-exact across all five transitions; the user's
   three screenshot artifacts each map to a fixed mechanism (a→merged
   lines/spliced chars, b→mid-word tearing, c→stale splices, 3b→stale
   selection rows).
5. **U004 fix — local sidecar in plain `tauri build`.** Stage the helper
   binary in the bare `--bundles app` output (tauri externalBin or a build
   hook mirroring `release/macos/build-package.sh`), and make the
   connect-failure banner print the exact build command when the helper is
   genuinely absent. Gate: both `pnpm … tauri build --bundles app` and the
   `release/macos` packaged flow connect locally on a fresh machine
   account (no prior install).
6. **Q005 re-measure, then decide.** After items 1-2, re-run the Phase 2
   flood lane on the shaped link (100 ms) and remote Linux host. The rejected
   continue's unscoped `%error` explains the observed resync; if the 60 s
   flood now shows zero connection-wide resyncs, record it and DEFER the
   12.2 ack-watermark/per-pane-overflow work to hardening backlog instead
   of building it here. Only if resyncs persist does that work enter this
   stage.
   **Measured 2026-08-14 — resyncs persist, so 12.2 is NOT deferred.** Local
   and the shaped 100 ms Docker lane are clean over 60 s (1.15 GB and 420 MB
   moved, zero connection-wide resyncs). remote Linux host is not: 60 s at 12.19 MB/s
   produced **271** connection-wide resyncs, every one of them
   `ResyncRequired full` — the host's event-queue overflow, i.e. the client
   cannot drain as fast as tmux produces. The U001 fix removed a different
   source of resyncs that had been masking this one. Per this item's own rule,
   the per-pane overflow recovery and ack watermark from 12.2 are the fix for
   P12-Q005 and are **still open**; 12.9 did not build them.
   **User decision 2026-08-14: out of scope.** The trigger is a pane
   sustaining ~12 MB/s over the real link — not an agent-session shape.
   P12-Q005 moves to the hardening backlog with the rest of the deferred
   12.2 work; it does not gate 12.9's exit or Phase 11's start.
7. **Full regression pass.** Re-run `run-perf.sh` both lanes,
   `run-stall.sh`, `run-pause-probe.sh`, host + desktop test suites, and
   the remote Linux host smoke. Budget table must not regress from `tests/performance/runtime`
   final measurements.

Exit: U001-U004 Fixed + Retested (lanes above green; done 2026-08-14),
Q005 deferred to the hardening backlog by user decision, and a second
user hands-on pass confirms agent-session feel — run a real claude/codex
session over SSH and watch the working counter tick. Phase 11 does not
start until that hands-on pass.

**Hands-on round 2 (2026-08-14) FAILED and opened §12.10.** The first
round-2 test unknowingly ran against a stale mid-run daemon on remote-linux
(fixed by installing the final helper and restarting daemons — flicker
dropped sharply, queue overflows stopped). The residual failure is
P12-U006 below.

### 12.10 Client-resize correctness — stop damaging real tmux windows

Root cause is verified (ledger: P12-U006, with U005 riding on it). The
app resized the **user's real tmux windows** to ~300 rows: `App.tsx`
`handleResize` scales the active pane's measured box by its topology
share (`rows = measured.rows * grid.height / pane.height`), which is
wrong whenever the box's actual share differs from the topology share —
pane zoom (box = full window, pane.height = split height) and transient
box/topology mismatch during layout changes. Field data matches exactly
(108x298/310/314 windows; width intact because the width ratio was 1).
Nothing clamps the request, `refresh-client -C` makes tmux obey, healing
only runs for the active pane of the active window, and every bad resize
emits a layout change → topology push → the remaining flicker.

1. **Replace the formula, don't patch it.** Compute the client size from
   the **tiled terminal surface**, not from any pane: measure the
   workspace surface's pixel box, divide by the renderer's cell metrics
   (same source FitAddon uses), send those cols/rows. Pane shares,
   zoom, and per-pane boxes must not appear anywhere in the computation.
   Debounce as today; send on connect, active-window change, and surface
   resize. The per-pane ResizeObserver keeps feeding `reconcilePaneGrid`
   (render side) but never the client-resize request.
   *Amended in flight (2026-08-14, measured):* every one of those triggers
   **recomputes**, but only a request that differs from the last one is
   sent. Re-sending a size tmux already has is not free — the lane's
   `identicalResend` probe measured 3 identical `refresh-client -C`
   requests producing **6 topology-dirty and 3 topology-snapshot events**,
   which is the "topology changed" churn this stage exists to remove.
2. **Clamp both ends.** Desktop refuses to request outside [2, 500] in
   either axis; host `resize()` tightens its validation from 1000 to 500
   and logs a diagnostic with the rejected size. A wrong future formula
   must produce a bounded error and a visible diagnostic, not a 300-row
   window.
3. **Tests that would have caught this:**
   - Unit: zoomed pane (box = full window, small pane.height), tiny
     2-column pane, and a box/topology mismatch mid-layout-change — in
     all three the requested size must equal the measured surface size,
     independent of pane geometry.
   - No-ratchet property: feed the computation its own output as the new
     grid for 10 rounds with panes of every share; the request must be a
     fixed point after round 1.
4. **Live verification against remote-linux (scratch only):** create
   `ade-phase12-geo` scratch sessions; attach a fixed-size second client
   (a control client that issued `refresh-client -C 188,51` works as a
   deterministic stand-in for a human terminal). Drive the app (or the
   relevant code path) through split, zoom, window switch, and layout
   churn. Assert over 5+ minutes: no window on the server ever exceeds
   the app's measured surface rows or 188x51-derived bounds, window
   sizes are stable (no monotone growth), topology pushes ≈ 0 while the
   layout is idle, and the host's `connectionsAccepted` delta over 10
   idle minutes is ≈ 0 (the ~72-connection churn seen in one short user
   session must be explained or gone).
5. **Regression:** desktop + host suites, `run-idle-steady.sh`,
   `run-pause-probe.sh`, local `run-perf.sh` lane; budget table must not
   regress. If host code changed, rebuild and reinstall the remote-linux
   helper (final artifact, `--allow-upgrade`) and restart its daemons
   before any remote measurement.
6. Update the ledger (U005, U006) with gate evidence.

Exit: item 3-5 lanes green, and a third user hands-on pass — claude/codex
over SSH with a plain terminal attached to the same session — shows no
cut-off panes, no window damage visible from the plain terminal, and no
topology-changed flicker. Phase 11 remains gated on that pass.

### Phase 12 exit criteria

1. All budget rows green on the physical Mac against B-Docker at 100 ms, with
   the harness (not RPC acks) as evidence; remote Linux host compatibility smoke shows
   the same character read-only.
2. Zero periodic idle round-trips; no full-connection resync under a 60 s
   flood; typing remains under budget during a concurrent bulk transfer
   (existing PRD requirement, now measured).
3. Widened Phase 2/Phase 8 bounds restored to their intended values and green.
4. Linux bounded regressions pass; helper upgrade path verified for the
   protocol/behavior changes; evidence, digests, and `implementation.md`
   entry complete; cleanup verified.

---

## Sequencing and handoff

1. **Run Phase 12 first.** Rationale: it is the daily-driver blocker; it is
   visually invisible (no conflict with the Phase 11 brief); and Phase 11's
   acceptance then inherits real latency budgets, so the redesigned UI is
   built and QA'd once, on the fast data path. The only overlap — App state
   decomposition, memoization, Explorer virtualization (12.6) — is
   structure-only and becomes the skeleton Phase 11 restyles.
2. Phase 11 then implements the approved mock on that baseline, with the
   Phase 12 harness re-run as a regression gate in its acceptance.
3. If the user prefers visible progress first, the phases can swap: Phase 11
   must then avoid touching the input/output data paths, and Phase 12 re-runs
   Phase 11's screenshot acceptance at close. The budgets and briefs above do
   not change in either order.
4. Each phase appends durable progress, review counts, commands/results,
   digests, limitations, and cleanup to `implementation.md`; evidence under
   `docs/history/qa/ui-polish/` and `tests/performance/runtime/`.
