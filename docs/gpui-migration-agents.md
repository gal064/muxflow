# GPUI migration: subagent execution plan

Status: **Phase 0 complete. The owner approved Phases 1 to 6 in `docs/gpui-autonomy-decisions.md`. See [Status](#status).**

**Done means:** a fully working GPUI app with feature parity, at the end of Phase 6, except for the exceptions listed in `docs/gpui-migration.md`.

This is how `docs/gpui-migration.md` gets executed. That document says what to
build and why, and this one says who builds it, in what order, and how it is checked.
A new session can resume from [Status](#status).

## Ground rules

- **Keep it simple.** Every task prefers the simplest code that keeps the behavior, and reviewers reject needless abstraction. Keeping the codebase small outranks schedule.
- **Root owns the result.** It dispatches, merges, resolves conflicts, updates this
  file and signs off each phase. It does not repeat reviews a worker already ran.
- **Base:** `gpui/integration`, in `../muxflow-gpui`, cut from an up-to-date
  `main`. Root merges `main` into it at the start of every phase.
- **Task worktrees:** `../muxflow-gpui-<task>` on branch `gpui/<task>`, cut
  from `gpui/integration` after a pull. Never under `/tmp`. Removed after merge, with
  build output cleaned.
- **Agents may push `gpui/*` branches to GitHub.** Merges to `main`, tags, releases and
  TestFlight are always the owner's call.
- **CI:** `ci.yml` and `gpui-spike.yml` run on pushes to `gpui/**`, on Linux and macOS.
- **Privacy:** this repository is public. Do not commit IP addresses, machine names, user
  names, home paths, container IDs or raw measurement output. Root runs `pnpm check:privacy` before each push.
- **Binding on every task:**
  - The license rules in `docs/gpui-migration.md`. Workers must not open Zed's GPL
    crates, `termua` or `rterminal`.
  - The compatibility rules in `CLAUDE.md`. No wire change is planned, so a task that
    finds it needs one stops and reports it.
  - `uv` for Python.
- **Ledger:** a task that ports, replaces or retires a module updates its row in
  `docs/gpui-migration-ledger.md`. Retiring needs a one-line reason.

## Stop rules

Root continues through Phases 1 to 6, and stops and notifies the owner only if:

1. A change to the wire protocol is necessary.
2. A dependency has a license that the plan does not allow.
3. A feature cannot reach parity and needs a new exception.
4. A test continues to fail after three fix attempts.
5. A task must touch a process or container that it did not start.

## Machine limits

- **One cargo command at a time** across all agents: put `~/dev/muxflow-gpui-tools/locked` first
  on `PATH`. Its `cargo` waits on a shared lock and builds with line tables only. Three parallel
  builds ran the machine out of memory. Run built binaries directly, never through `cargo run`.
- Each worktree has its own `target`. Root deletes it after merging.
- A parallel task's QA must be headless and isolated: its own tmux socket
  (`tmux -L muxflow-qa-<task>`) and its own `XDG_RUNTIME_DIR`. Workers must not touch
  a host daemon or tmux server that the task did not start.
- Kill any dev daemon a task started before it finishes.
- Any QA that launches the app or helper must run it inside `spikes/gpui/qa-sandbox.sh`. Start
  Xvfb and every X tool inside it too. Build outside it. No X tool runs outside it.
- Any QA that launches the app must unset `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`,
  `XDG_STATE_HOME`, `DISPLAY`, `WAYLAND_DISPLAY` and `DBUS_SESSION_BUS_ADDRESS` first, then
  set a private `HOME`. The app honors exported `XDG_*` values over `HOME`.
- Spikes reach a display only through `spikes/gpui/env.sh`. Never use `DISPLAY=:0`,
  `WAYLAND_DISPLAY`, or start an X server that picks its own display number.
- Do not touch any process, container or window that the task did not start.
- Workers may install user-level tools (`cargo install`), and
  may unpack Arch's `vulkan-swrast` into a private directory and point `VK_DRIVER_FILES`
  at it for headless rendering. System packages are not changed.

## Modes

| Mode | Owner of implementation | Owner of review and QA |
|---|---|---|
| **Full cycle** | Ordinary implementation agent, told to load the `gdev_cycle` skill and run autonomously | That same worker: 1 to 3 review rounds (small/medium) or up to 5 (large), its `gdev_qa_*` QA agent, then its commit |
| **Implementation only** | Ordinary implementation agent, with its lightweight tests and commit. Not given `gdev_cycle` | The later task named in its row |

"Invariant" means the worker adds an invariant reviewer. "Design" means it adds a
design reviewer that renders the UI, as `gdev_cycle` requires for that kind of work.

Every phase ends with root merging in dependency order and a fresh ordinary subagent
named `gdev_qa_phase<N>` running the integrated QA listed for that phase. Root fixes
what it finds, or sends the task back.

## Phase 0: Feasibility (go or no-go)

**Linux only:** the owner has scoped Phase 0 to Hyprland and X11. GNOME and macOS checks
are deferred, and macOS must be re-checked before Phase 2 starts. T0.7 builds the spike
on the macOS CI runner, so at least compile problems surface now.

Everything lives in a disposable workspace, `spikes/gpui/`, with its own `Cargo.lock`,
so the shipping workspace is untouched. Only T0.1, T0.2, T0.6 and T0.7 touch anything
outside it, and none of those change the shipping app.

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T0.1 `license-gate` | Full | sonnet, medium | `deny.toml`, `.github/workflows/ci.yml`, `CLAUDE.md`, `AGENTS.md` | none |
| 1 | T0.2 `ledger` | Impl only, reviewed in T0.7 | sonnet, medium | `docs/gpui-migration-ledger.md` | none |
| 2 | T0.3 `spike-scaffold` | Full | sonnet, high | `spikes/gpui/**` | T0.1 |
| 3 | T0.4 `spike-terminal` | Full, large, invariant | opus, high | `spikes/gpui/terminal/**` | T0.3 |
| 3 | T0.5 `spike-markdown` | Full, design | opus, high | `spikes/gpui/markdown/**` | T0.3 |
| 3 | T0.6 `vt-parity` | Full | opus, high | `tests/performance/runtime/perf-driver/**`, `run-vt-parity.sh` | none in code |
| 4 | T0.7 `spike-ci-baseline` | Full | sonnet, high | `.github/workflows/ci.yml`, baseline script under `tests/performance/runtime/` | T0.1, T0.4 |

| Task | Scope | Acceptance | Lightweight tests | Worker QA |
|---|---|---|---|---|
| T0.1 | `cargo deny` with a permissive-only allowlist, in CI. Add the no-GPL rule and pin policy to `CLAUDE.md` and `AGENTS.md`, keeping them identical | Passes on the current tree, and fails when a GPL crate is added | `cargo deny check licenses` | Add a known GPL crate on a scratch copy and confirm the failure |
| T0.2 | Generate one row per file in `apps/desktop/src`, with port, replace or retire. Includes S8: classify each `draggable` use as in-app or OS-bound | Every tracked module and test is listed. S8 answered | Script compares the rows with `git ls-files` | None (T0.7) |
| T0.3 | Pin `gpui-kit`, confirm it builds on Rust 1.97.1 (S6), and open a themed window with JetBrains Mono. Measure whether `.ttf` is needed | Builds with exact pins, and `cargo deny` passes for the spike | `cargo build`, `cargo deny` | Launch under `Xvfb` with software Vulkan and capture a frame, or report that it cannot run |
| T0.4 | S1, S2 and the terminal half of S5: panes fed by `crates/tmux-control` through `alacritty_terminal`, a custom element, flood and resize | Reports p95 key-to-glyph, flood and memory numbers. Does not read GPL or AGPL terminals | `cargo test` for the feed and damage logic | Isolated tmux, 8-pane flood headless, numbers recorded in the report |
| T0.5 | S4 and the editor half of S5: hidden syntax and an editable inline table on the Kit editor or `gpui-base` text | A clear works or doesn't answer, with what it would cost | `cargo test` for the mapping logic | Headless render of a real Markdown fixture, screenshots attached |
| T0.6 | S9: add `alacritty_terminal` as a third parser in the VT parity lane, with history-page autowrap | Passes, or lists each divergence | `cargo test` in `perf-driver` | Run `run-vt-parity.sh` on an isolated socket |
| T0.7 | S7, a macOS CI build of the spike, Rust 1.97.1, and a black-box key-to-pixels baseline of the Tauri renderer on the T0.4 scenarios. Reviews T0.2 | GPUI tests and pixel captures run on Linux CI, or the blocker is named. The ledger is verified | CI workflow lint | Run the headless path as CI would |

**Integrated QA, `gdev_qa_phase0`, then root:**
- Merge everything, run the spike against the isolated baseline, and compare numbers.
- Root runs S3 and S10 locally, driving the spike with `cua-driver`
  (input goes through X11, reads through AT-SPI). Use Xwayland for X11 and a Hyprland
  headless output for native Wayland. Run one app at a time, after merging.
- Latency compares the spike and the Tauri baseline on the same output, so the result
  is relative. Absolute frame timing is not representative of a physical display.
- IME: prove
  composition with a stub input method in a private compositor if one is workable.
  Otherwise S5 is reported as not proven, and it does not block a Linux result.

**Stop here.** Root writes the go or no-go report with measurements, and the owner
decides. The owner approved Phases 1 to 6.

## Phase 1: `desktop-core`

Task 1.3 changes most of `src-tauri`, so it cannot be parallelized with anything else.

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T1.1 `golden-paths` | Full | sonnet, medium | New test module in `src-tauri` | Go |
| 1 | T1.2 `prefs-to-app-state` | Full | sonnet, high | `app_state.rs`, `ui/appearance.ts`, `features/agents/sound.ts` | Go |
| 2 | T1.3 `core-traits` | Full, large, invariant | opus, high | `src-tauri/src/**` | T1.1, T1.2 |
| 3 | T1.4 `core-crate` | Full, large, invariant | opus, high | Moves to `crates/desktop-core`, repoints scripts | T1.3 |

| Task | Acceptance | Lightweight tests | Worker QA |
|---|---|---|---|
| T1.1 | Tauri-resolved paths recorded for both platforms | The test itself, on Linux and macOS CI | None: test only |
| T1.2 | The three keys persist in `app_state`, and existing values carry over | Rust and vitest for the store and import | Run the Tauri app against scratch state and confirm the import |
| T1.3 | `Channel`, `emit`, `AppHandle` and `State` are confined to the command shims | `cargo test -p muxflow` | `pnpm test:transfers:local` on an isolated socket |
| T1.4 | `cargo tree -p desktop-core` has no `tauri`, and behavior is unchanged | `cargo test --workspace` | `pnpm test:transport:local`, `pnpm test:lifecycle` |

**Integrated QA:** `gdev_qa_phase1` runs the existing `pnpm test`, `cargo test --workspace`,
`test:files`, `test:git`, `test:agents` and a packaged Tauri smoke run.

This phase can reach `main` ahead of the rest, because the T1.2 release must precede
cutover. The owner decides when.

## Phase 2: Native shell

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T2.1 `app-foundation` | Full, large, design | opus, high | `apps/desktop-gpui` skeleton, theme, fonts, palette generator in `packages/client-core` | Phase 1 |
| 2 | T2.2 `chrome-layout` | Full, design | opus, high | `ui/chrome/**`: dock, sidebar, tabs, right panel | T2.1 |
| 2 | T2.3 `commands-keymap` | Full | opus, high | `commands/**`: registry, keymap, overrides, palette, Quick Open | T2.1 |
| 2 | T2.4 `connections` | Full, invariant | opus, high | `connection_ui/**`: profiles, setup, helper recovery | T2.1 |
| 3 | T2.5 `native-surfaces` | Full, design | sonnet, high | Menus, notices, tooltips, context menus, window geometry, `macos_window`, Linux decorations | T2.2 |

T2.1 must publish the module seams and a single feature-registration point, so wave 2
tasks do not collide in the app root. It also sets the AccessKit baseline every later
UI task follows, because the packaged `cua-driver` journeys read the accessibility tree.

**Module ownership in `apps/desktop-gpui/src`:**

| Task | Owns | Joins the app through |
|---|---|---|
| T2.1 | `app.rs`, `features.rs`, `theme/**`, `fonts.rs`, `paths.rs`, `ui/mod.rs`, `ui/shell.rs` | |
| T2.2 | `ui/chrome/**`, and `ui/title_bar.rs` for its height and control placement (tabs in the title bar) | `chrome::Chrome` (rendered by the shell) and `Chrome::title_bar_content` |
| T2.3 | `commands/**` | `commands::init` |
| T2.4 | `connection_ui/**` | `connection_ui::init` |
| T2.5 | Linux decoration selection and macOS details in `ui/title_bar.rs`, window options in `app.rs` | Its own new modules, each added with one line in `features.rs` |

`features::init` in `features.rs` is the single feature-registration point. Wave 2 tasks
fill their own module and need no change in the app root. Later phases add one line there.
The accessibility baseline is the module comment in `ui/mod.rs`: every control has a
stable id, an AccessKit role and a name.

**Tests and QA:** each task runs `cargo test -p muxflow-gpui` on its module. Worker QA
is headless, driving GPUI's test context and capturing frames.

**Integrated QA:** `gdev_qa_phase2` connects to a local host and a Docker SSH host,
restores tabs as placeholders, and confirms all 64 commands appear.

## Phase 3: Terminal

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T3.1 `terminal-engine` | Full, large, invariant | opus, high | `crates/terminal-engine` | Phase 2, T0.4 |
| 1 | T3.2 `terminal-interactions` | Full | sonnet, high | `crates/terminal-interactions`, fixtures, `packages/terminal-interactions` tests | none |
| 2 | T3.3 `terminal-element` | Full, large, invariant, design | opus, high | `terminal/element.rs`, sizing | T3.1 |
| 3 | T3.4 `terminal-input` | Full, invariant | opus, high | `terminal/input/**` | T3.3 |
| 3 | T3.5 `terminal-reading` | Full | opus, high | `terminal/reading/**`: selection, search, links, history | T3.3, T3.2 |
| 3 | T3.6 `terminal-integration` | Full, invariant | opus, high | `terminal/integration/**`: OSC 52, paste, drag and drop, transfers, routing, colors, screen reader | T3.3 |
| 4 | T3.7 `seed-fixes` | Full | sonnet, high | `apps/host/src/service/terminal/seed.rs` | none |
| 4 | T3.8 `desktop-paste` | Full, invariant | opus, high | desktop-core input dispatch, `terminal/input` paste, `connection_ui/backend.rs` | T3.4 |

T3.3 defines the pane entity and the three submodule seams, so wave 3 stays apart.

| Task | Acceptance | Lightweight tests | Worker QA |
|---|---|---|---|
| T3.1 | Generations, seeds, backlog and health match the TS behavior | Ported hub and pager tests | Replay recorded tmux fixtures |
| T3.2 | Shared fixtures pass in both Rust and vitest | `cargo test`, `pnpm mobile:test` | None: pure logic |
| T3.3 | Exact tmux client size, damage-based repaint | Cell metric and row cache tests | Headless flood on an isolated socket, frame captures |
| T3.4 | `terminalInputPolicy` ported, Ctrl+/ sends `0x1f`, mouse and bracketed paste work | Table tests from the TS suite | Scripted input into a real TUI on an isolated socket |
| T3.5 | Copy cleanup, `~` links and history paging match today | Ported selection and paging tests | Scripted selection and search against tmux |
| T3.6 | Screenshot paste works over SSH, and `TerminalColors` is sent | Unit tests per surface | Docker SSH paste with an isolated socket |
| T3.7 | Host seeds place trailing-cell overlays at the right columns when `capture-pane -N` does not pad the line, and seed mouse mode from `mouse_all_flag` | Seed unit tests from real captures | Seed a pane with a mid-line background run and a click-only mouse app on an isolated socket |
| T3.8 | A desktop paste goes through the host's paste path, as on mobile, ordered with the keys around it | desktop-core dispatch tests | Multi-line paste into a bracketed-paste program on tmux 3.4 and 3.5 in the sandbox |

**Integrated QA:** `gdev_qa_phase3` runs Claude Code and Codex locally and over SSH, the
VT parity lane at all five quiesce points, and resizing during heavy output.

### Carry-forward from Phases 2 and 3

The phase that owns each area fixes these; the Phase 6 parity check confirms them.
T6.6 closed the rest: fixed, already fixed, or matching Tauri (its commit lists each).

- X11 window position is not restored (owner informed).
- Phase 5: the context-menu key (Shift+F10, Menu) in the Explorer and Git rows, and keyboard row access in Git. Fixed (T6.1).
- Phase 5: Git push errors show the host's raw text, not the SurfaceError summary with Details (T6.4).
- Phase 5: Kit bug, double-click selects nothing in a preview paragraph with inline code (report upstream). Drag-select in the preview is on the owner's Mac prompt.

## Phase 4: Workspaces and panels

All run in one wave, at most 3 at a time. Each owns its feature module and registers
through the T2.1 seam.

| Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|
| T4.1 `workspaces-shell` | opus, high, full cycle, large, design | `gpui/workspaces-shell` | Merged via T4.5 | `7cf0fee` | 5 review rounds. Its QA failed 4 runs on focus; the last fix was re-checked by T4.5's QA. Fixed the revealVoid false positive in terminal-engine |
| T4.2 `agents-automations` | opus, high, full cycle, invariant | `gpui/agents-automations` | Merged via T4.5 | `4f175dc` | 2 review rounds. Adds rodio, so CI needs `libasound2-dev`. Agent launch and resume need Phase 5's `ActiveRoot` |
| T4.3 `ports-transfers` | Full | sonnet, high | `features/ports`, `transfers`, `dragDrop` | Phase 3 |
| T4.4 `settings-diagnostics` | sonnet, high, full cycle, design | `gpui/settings-diagnostics` | Merged via T4.5 | `e6e8510` | 1 review round, QA pass. The profile label rename and the tmux identity notice match Tauri |
| T4.5 `phase4-wiring` | opus, high, full cycle, invariant | `gpui/phase4-wiring` | Merged | `ee8358d` | Merged T4.1, T4.2, T4.4 and joined their seams. 1 review round, QA pass after 2 fixes (a pane move selects its window; a startup command is retried while the bridge reconnects). Open: bulk-close topology authority, `agent.state` diagnostics |
| T4.3 `ports-transfers` | sonnet, high, full cycle | `gpui/ports-transfers` | Merged | `daa7af4` | 2 reviewers, 1 round. QA pass on local and SSH hosts; document-tab drags, the dock zones and the agent menu were not run live (headless tests only). Open: upload Cancel waits for the host's response before it reports |
| `gdev_qa_phase4` | fresh QA | `gpui/integration` | Passed after fixes | | All panels against local and SSH hosts. Found a notification click leaving tmux focus on the old pane, and a tooltip over a context menu |
| T4.6 `phase4-fixes` | opus, high, full cycle, invariant | `gpui/phase4-fixes` | Merged | `0912995` | Fixed both QA bugs (cross-host navigation was cancelled by a connection change). Ported the topology authority, so Close All Non-Agent Tabs works. No Phase 4 ledger rows left open except the Phase 5 deferrals |
| T5.1 `explorer` | opus, high, full cycle, invariant | `gpui/explorer` | Merged | `6604380` | 3 review rounds, QA PASS local and SSH after one dialog-focus crash fix. Wired the editor opener, watcher and download, `ActiveRoot` and Git ignored paths. One shared watch registry serves the tree and open documents |
| T5.2 `editor` | opus, high, full cycle, large, design | `gpui/editor` | Merged | `1e4a659`, merge `e372bc2` | Kit code editor. 3 review rounds, QA PASS local and SSH. An external edit to an open remote file leaves the tab in Save failed until T5.1 delivers change notices |
| T5.3 `git-diff` | opus, high, full cycle, design | `gpui/git-diff` | Merged | `ef076e7`, merge `9a9ad34` | 3 review rounds, QA PASS local and SSH, `test:git` passes after a driver fix. Gaps: SurfaceError summary on push errors, keyboard row access, file icons |
| `gdev_qa_phase5` | fresh QA | `gpui/integration` | Passed | `ec39e80` | All 10 steps on local and SSH hosts: browse, edit, external change, Markdown, file operations, diff, stage, commit, push to a scratch remote, gitignore, reconnect, relaunch. Minor: Go to Line leaves the line at the bottom edge; Git panel blank instead of "Working tree clean." when only ignored files exist; the docked tab can sit in the overflow menu (T6.6: the first two fixed; the third matches Tauri) |
| T6.4 `ledger-shell` | opus, high, full cycle, invariant | `gpui/ledger-shell` | Merged | `6906fa0` | 55 Phase 2 rows closed; `ui/ContextMenu.tsx` left for T6.1. SurfaceError on every surface, update check, loading delay, reduced motion, icons, narrow width. 2 review rounds, QA pass. Deviation: Kit's dialog keeps a 16 px gutter below 600 px (Tauri 8 px) |
| T6.5 `ledger-terminal-files` | opus, high, full cycle, invariant | `gpui/ledger-terminal-files` | Merged | `f68af3a` | 38 Phase 3 and 5 rows closed. Ported the perf probe (same perf-log schema), fixed a paint-gate arming bug. 3 review rounds, QA pass incl. restore, stale restore, reseed, both perf scripts |
| T6.6 `carry-forward` | opus, high, full cycle | `gpui/carry-forward` | Merged | `9839d9c` | Fixed paste ordering, the first-workspace identity notice, Settings Live Markdown copy, Working tree clean, Go to Line centering, disabled Delete styling, rename focus, `xdg-open` reaping. The rest verified fixed or matching Tauri. 1 review round, QA pass |
| T6.3 `behavior-docs` | sonnet, medium, full cycle, docs only | `gpui/behavior-docs` | Merged | `2f6ebe0` | `docs/gpui-behavior-audit.md`: 116 statements hold, 5 differ or are missing: 3 keyboard gaps routed to T6.1, stale source paths in `mac-ssh-hosts.md` (cutover edit), and the incident journal. Mac items for the owner prompt, cutover edits listed. The incident-journal gap was fixed at the root (`674fc7e`) |
| T6.7 `parity-fixes` | opus, high, full cycle | `gpui/parity-fixes` | Merged | `26d930f` | Fixed the transient "host bridge closed" notice and the 0.0.0 app version. Ports after a host switch, the New workspace default host, the blank terminal after relaunch and the attention-generation toast all match Tauri (checked side by side). QA pass |
| T6.2 `visual-parity` | sonnet, high, full cycle, design | `gpui/visual-parity` | Merged | `c5187a5` | Fixed a diff crash across 900 px, scrim, toasts, setup dialog, Settings Connection tab, split Commit, Markdown source colours, diff row colours, palette band. Accepted (Kit or rasterization): heading letter-spacing, textarea grip, hatched diff fillers, line pitch within 1 px, editor gutter and find widget, context-menu row size, preview mid-word breaks, radio size, dialog width below 600 px. Open: tab strip pitch (Tauri reference build was stale), preview a few px short |
| T6.1 `accessibility` | opus, high, full cycle, large, invariant | `gpui/accessibility` | Merged | `a000c07` | Keyboard context menus everywhere Tauri has them (Shift+F10, Menu key, Home/End, Tab trap, open on checked item), roving and per-row Tab stops as in Tauri, focus return after menus and dialogs, posinset fix, named alerts and groups. 4 review rounds, QA pass. Volume slider fixed in T6.8. Kit/AccessKit gaps recorded in `ui/mod.rs` |
| T6.8 `final-polish` | opus, high, full cycle | `gpui/final-polish` | Merged | `805c07f` | Keyboard volume slider (saved on every change), tab pitch from the current CSS, confirmation width 620 px. Accepted: Markdown preview text clips 48 px below the top and 80 px above the bottom while scrolling (Kit pads outside its virtualized list; an unvirtualized fix cost 4 to 20 ms a frame), and Kit's 16 px dialog gutter below 600 px. Drag-select in editor and preview fails under xdotool only; on the Mac prompt |
| `gdev_qa_phase6` | fresh QA | `gpui/integration` | Passed | `f93e815` | Release build, local and SSH journeys, relaunch restore, both perf scripts, all Rust and frontend gates, CI green on Linux and macOS. Acceptance lanes needing Docker-published ports, 25 GiB scratch or a Tauri package build not run. Pre-existing on `main`, not the port: the source scan looked for a README line that moved to `docs/setup.md` (fixed here), and the `phase6-ssh` agents lane fails on a renamed field and a lifecycle expectation (not run in CI) |
| T6.9 `mac-fixes` | opus, high, full cycle | `gpui/mac-fixes` | Committed | | The owner's Mac test. Fixed: Home/End sent as tmux's keys, Ctrl+Enter commits on macOS, the agent row's host letter and full width with its actions over it, the test notification button, the notification prompt at launch, the palette search field role, no "Disconnected" strip while the first tmux server is adopted, the Replace All tooltip, unknown key names refused in the shortcut editor, one full-screen item in the macOS View menu, app shortcuts from text fields (except the field's own chords), a named Settings section list. The blank diff tab after a commit did not reproduce on Linux; chasing it found a host bug that staged an untracked file's hunk as the file's absolute path (Git read the pinned `/proc/self/fd` path as a symlink), fixed by passing the content on stdin. Accepted: while the bridge reattaches to the first tmux server, a real drop shows only once that attempt fails; a server started during the handshake still flashes the strip |
| T5.4 `markdown` | sonnet, high, full cycle, design | `gpui/markdown` | Merged | `587887b` | 2 review rounds, QA PASS local. Kit bug: double-click selects nothing in a paragraph with inline code. Drag-select in the preview not verified (xdotool). Settings copy still mentions Live Markdown |
| T7.1 `live-md-core` | opus, high, full cycle, large, design | `gpui/live-md-core` | Merged | `cce623c` | Crate `live-markdown` from the spike; Live mode through `editor::set_live_markdown`, editing the tab's document buffer. Live limit is a temporary 48 KiB (Tauri 1 MiB; T7.5 restores it). Measured, release, software rendering, table-heavy Markdown, key and frame p95: 25 KB 8.3 and 3.1 ms, 50 KB 21.6 and 11.7 ms, 75 KB 41.4 and 23.6 ms, 100 KB 60.6 and 35.1 ms, 150 KB 319 and 246 ms, 250 KB 1268 and 1089 ms, 500 KB about 2.9 and 2.55 s. Causes: every block laid out each frame, a whole-file reparse per key (about 0.17 ms per KB), and `gpui-base` 0.9.0's retained-layout sweep going quadratic past 4096 entries (`src/text/inline.rs:480`). 2 code and 2 invariant review rounds, 2 design rounds. Fixed: Live edits lost when the source editor was made later, an IME commit by unmarking, a cancelled composition dirtying the file, a refused edit staying on screen, Live's view and undo dropped on a tab switch, app shortcuts taking Copy, Paste and Find from a focused Live view, heading padding, the closing fence's row, inline-code backticks, the rule's position, the Settings hint. QA PASS local and SSH, recheck PASS. For T7.2: a drawn task box (font fallback draws the checked one as emoji), undo across an outside change, double-click word selection, a caret stop on a rule |
| T7.2 `live-md-blocks` | opus, high, full cycle | `gpui/live-md-blocks` | Merged | `8e5a38c` | Setext headings, nested quotes, autolinks, shortcut references, image alt text; list marker column with hanging text; Tab/Shift-Tab and Ctrl+]/[ with renumbering, Enter continuing and renumbering, ending a list, continuing a quote at its depth or an item's later paragraph; Ctrl+B/I; drawn task boxes toggled by click; Ctrl+click links through the preview's `follow`; word motion, double- and triple-click, visual-line Home/End, a caret stop on a rule; undo mapped across an outside change (dropped when the change overlaps an edit). New `lists.rs`: lists parse as lezer reads them (an empty item, or one not numbered 1, may break into a paragraph inside a list), which removed a renumbering workaround. The fence's language label stays visible, as in Tauri. Not ported: Tab/Ctrl+]/[ outside a list, Ctrl+click on a definition's URL. 3 review rounds, QA FAIL then recheck PASS except: a caret put at a wrap boundary other than by Home (click, undo) draws at the end of the line above. markdown-rs 1.0.0 panicked on a list item ending in unclosed code or HTML followed by another item, crashing Live and, on every relaunch, Preview and Split: fixed in `vendor/markdown` (upstream's open PR #267, see its `VENDORED.md`); a 3-minute fuzz over 9 million inputs with Live's and Preview's options found no other panic. `parse()` keeps a backstop that draws the source as plain lines until an edit parses, reporting a panic once per stretch; Preview's parse in gpui-base has no such backstop. Merged with T7.3: one set of Ctrl+B/I actions (a cell's rules first), Tab tries the table then the list, any click on a missing cell enters it, and Ctrl+B/I again in a cell unwraps markers just outside the selection, as prose does. |
| T7.3 `live-md-tables` | opus, high, full cycle | `gpui/live-md-tables` | Merged | `9a5b236` | Full `liveTable` parity in crates/live-markdown: cell writes (trailing backslash's space), Tab and Shift-Tab, Enter, arrows at cell edges and into a table, Escape, adding and removing rows, Ctrl+B/I in a cell, short rows drawn to the header's width (missing cells take the caret and fill the row when typed into), extra cells not drawn, read-only tables, the focus ring (rebuilt from gpui-base's column sharing). All `liveTable.test.ts` cases ported. 1 code review round (fixed Home/End in a missing cell, a non-breaking space shifting the Ctrl+B selection). QA local and SSH: pass except the ring, fixed and rechecked. Open for the owner: a table wider than its column floors (about 11 columns) clips where Tauri's scrolls, and a clipped cell can take the caret unseen; a space typed at a cell's end is written at once (Tauri trims it unless more text follows) |
| T7.5 `live-md-viewport` | opus, high, full cycle, large, design | `gpui/live-md-viewport` | Merged | `78f65c0` | Live lays out and paints only the blocks near the viewport (`view/viewport.rs`: a scroll anchor, measured and estimated heights, a scrollbar, Page Up and Down, drag past the edges), projects only the top-level nodes an edit or the selection reaches (`edit/drawn.rs`), and parses only a window around each change, checked at both ends, with a whole parse as fallback; a new tree is built a piece at a time (`reparse.rs`). The host takes the view's changed span instead of diffing the buffer. Limit back to 1 MiB. Release, software rendering, 1 MiB, key p95 at top, middle and end: prose 8.5, 6.1 and 1.9 ms, tables 14.2, 16.0 and 8.3 ms; open 0.5 to 1 s on the UI thread. Slow, documented in the README: one giant list or quote (0.5 to 0.9 s a key), a fence open above most of the file, definition changes, files with thousands of definitions (about 30 ms a key) |
| T7.6 `live-md-wide-tables` | opus, high, full cycle | `gpui/live-md-wide-tables` | Merged | `774a116` | A table wider than its column's gpui-base floors (about 11 columns) scrolls sideways inside its frame instead of clipping (`view/wide.rs`): Live lays it out at the sum of the floors, measured as gpui-base measures them (the 14/15 table size, the semibold head, bold, italic, inline code in mono at 7/8 with its padding), and moves it inside the column. The caret's cell follows the caret and the view's edits (arrows, Tab, click, typing, undo); a sideways wheel or swipe, or Shift and the wheel, scrolls it, one axis at a time. Scroll kept per table in `LiveDoc` by the table's start, mapped through edits, dropped with the table; a change spanning the start resets it. Hidden cells take no click; the caret and ring are clipped to the table. Hook for T7.4: `LiveView::reveal_in_table(pos)`, which also lays out that table off screen. 2 code review rounds (fixed: inline code measured too narrow, a dead quote inset, trackpad drift, the hook off screen). QA local and SSH: 12 of 13 pass; a click on a clipped cell's visible padding went to its neighbour, fixed and rechecked. Open for the owner: no horizontal scrollbar under a wide table (Tauri has one); scrolled columns sit at gpui-base's 4 rem floors, so short words break mid-word (Tauri keeps a column at its longest word) |
| T7.4 `live-md-find` | opus, high, full cycle | `gpui/live-md-find` | Merged | `91b04f9` | Ctrl+F (⌘F) in the Live view: the source editor's find bar, one UI, drives either editor through a `Target` (the source editor, or the Live view through the new `editor::LiveFind` on the Live seam); matching, counting, stepping and replacing stay in `editor/find.rs`. The view marks the matches, the current one apart, and the selection's other occurrences (`highlightSelectionMatches` defaults) only in the blocks it draws (new crate `find.rs`), in Tauri's colours; a match in syntax hidden while the bar has focus is counted and selected but not marked, and its place scrolls into view. Replace and Replace All are one Live undo step each and autosave. Ctrl+F/Ctrl+H bound in the Live view; its Escape is Kit's, so it closes the bar. Release, software rendering, 1 MiB prose: find on a common word opens in about 6 ms, a step 3.3 ms p95, a jump to a block never drawn about 10 ms, a key with the bar open 4.4 ms p95. 3 code review rounds: fixed whole-node scans per frame, clicks on the bar falling through to the view (QA found it too), regex matches past a line end not marked, a reveal into hidden markup, the matcher re-sent per key. QA local and SSH PASS but the click-through, fixed and rechecked PASS. Differences kept from Tauri, from sharing the source editor's bar: typing a query selects the first match (Monaco), the bar overlays the page's top right instead of pushing it down, no Mod-G; Replace All leaves the caret at the first replaced match when it was inside the replaced stretch. After T7.6: a match in a wide table scrolls the table to it through the caret's cell following (tested), Replace there keeps it in view; while the bar is open a revealed match comes in below the bar's measured height (`LiveView::set_top_inset`), which also gives the page's top that much room; T7.6's unused `reveal_in_table` hook removed. Replace All resets a wide table lying inside the replaced stretch to its left edge, as an outside change does. Merge review sound; QA after the merge PASS |
| T7.7 `live-md-polish` | opus, high, full cycle | `gpui/live-md-polish` | Merged | `883450b` | Phase 7 bug-fix pass. Fixed, each with a test: a pointer or IME position from the frame drawn before the text shrank is clamped to the text and a character boundary (`LiveDoc::clamp`, in `snap` and the view's hit) instead of panicking; Down and Up judge a probe by where the caret would stop, so a hit on the bullet beside the caret's own line no longer sticks at a list's first or nested item; Ctrl+Home/End (⌘↑/⌘↓) and Shift to select; the page scrollbar takes a press before a hover has shown it (the view drags it, keeping the grab point); IME bounds fall back to the caret when the selection starts off screen; a scrolled wide table has a draggable bar under it and no column narrower than its longest word plus a space (gpui-base grows all columns alike, so a column with short words but long text can be wider than Tauri's); a save that failed while disconnected is tried again on reconnect (Tauri has the same gap). Not a bug: the "blue" table `1` is a subpixel colour fringe on a one-pixel stem, as on any glyph. Reported, not fixed: Quick Open keeps the first row it selected before file results arrive (same in Tauri and at `403d063`; an 8-line fix would pin only a selection the user moved); Preview drops an empty list item (gpui-base's `render_list_item` draws no marker without children, at `403d063` too; Tauri draws it); the blank Live view behind Settings did not reproduce, cached or not; held drag past the edge does not keep scrolling (needs a timer); a paste past 1 MiB keeps the tab Live with undo, as Tauri, which checks only the size it read; the caret at a wrap boundary after undo or a click. Found by QA, pre-existing (T7.2): Tab on an empty last list item before a paragraph leaves the caret on the blank line after it. 1 code review round (fixed: a press on a thumb jumped it to centre). QA local and SSH PASS; IME placement not run (no IME that follows the caret in the sandbox) |
| T7.8 `live-md-tabfix` | opus, high, small cycle | `gpui/live-md-tabfix` | Merged | `a0f58ad` | Tab on an empty last item before a paragraph (T7.7's QA find) left the caret on the blank line after it. Cause: markdown-rs ends a nested item before a blank line past its line break, and an empty item's bullet took that break, so its only stop was on the next line. The bullet now ends at its line's end (`projection.rs`); tested for bullet and ordered lists, with and without text after. Nested empty items before blank lines also draw each blank line as a row again. 1 code review round (fixed a comment, a stray blank line). QA local PASS. |
| T7.9 `hidden-cpu` | opus, high, implement + 1 review round, Linux measurement | `gpui/hidden-cpu` | Merged | `837c9eb` | GPUI already drew nothing while minimized: gpui-pre's X11 (unmapped or fully obscured), Wayland (xdg `suspended`) and macOS (occlusion) backends stop the frame source. What ran were polls that only feed the screen, each costing a Timer-thread, pool-worker and main-thread wakeup: the terminal link's drain (16 ms idle, 4 ms while output flows), the ports panel (200 ms) and the cursor blink (600 ms). New `ui::visibility` flag follows the window; while hidden the link drains every 100 ms (16 ms if a drain handed back over 512 KiB or 256 frames of host credit, so tmux's `pause-after` is never reached), ports check every 1 s, blink runs only for a focused pane in the active window; showing redraws in full and restarts both polls. Linux sandbox (Xvfb + openbox, lavapipe, release, 6 agent panes, minimized via WM iconify): working and silent 1.12% CPU, 359 wakeups/s to 0.20%, 77/s; printing about 10 lines/s each 3.80%, 1902/s to 0.99%, 392/s; 0 frames either way; visible unchanged; `seq 1 3000000` not slower minimized. Restore shows text and agent states written while minimized at the first frame (screenshot). Left: the 100 ms drain, the 250 ms notification-activation poll (needed while minimized), the main-thread frame pump and the engine/host threads reading output. Tests: `ui::visibility::tests::the_flag_follows_the_window`, `connection_ui::controller::tests::a_hidden_window_drains_the_engine_at_the_hidden_pace`. Review fixed: hidden drain throttling heavy output, a hidden-pace drain outliving the show, ports stale for 1 s after show, a wrong comment. macOS not measured yet (prompt handed to the owner's Mac agent) |
| T8.2 `macos-package` | opus, high, implement + 1 review round + CI package test | `gpui/macos-package` | Committed | | `build-package.sh` assembles `Muxflow.app` by hand from `muxflow-gpui` (shipped as `muxflow`) and `muxflow-host`, with the released Info.plist keys from a checked-in `release/macos/Info.plist`; no `pnpm` or `tauri build`. Same output paths, signing, notarization and DMG. CI job `macos-package` builds, verifies and launches the ad-hoc package. Owner Mac test: `target/t82/mac-package-test.md` |

**Tests:** ported logic tests per module. **Worker QA:** drive the panel against an
isolated host, headless. **Integrated QA:** `gdev_qa_phase4` exercises every panel
against a real host and confirms those ledger rows are closed.

## Phase 5: Files, editor, Git

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T5.1 `explorer` | Full, invariant | opus, high | `features/files` tree, cache, watches | Phase 4 |
| 1 | T5.2 `editor` | Full, large, design | opus, high | Editor tabs, autosave, grammars | Phase 4 |
| 1 | T5.3 `git-diff` | Full, design | opus, high | `features/git`, diff | Phase 4 |
| 2 | T5.4 `markdown` | Full, design | sonnet, high | Markdown in the source editor with the rendered preview | T5.2 |

**Tests:** ported listing, autosave and search tests, plus `cargo deny` for each
grammar. **Worker QA:** T5.1 expands a 4,096-entry directory, T5.2 runs a save conflict
on a remote file, T5.3 checks alignment at 320 px, and T5.4 edits a real Markdown file.

**Integrated QA:** `gdev_qa_phase5` browses, edits, diffs, stages, commits and pushes
against a scratch remote over Docker SSH.

## Phase 6: Parity check

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T6.4 `ledger-shell` | Full, invariant | opus, high | Close the open Phase 2 ledger rows: verify each against code and tests, port what is missing, and SurfaceError on every surface | Phase 5 |
| 1 | T6.5 `ledger-terminal-files` | Full, invariant | opus, high | Close the open Phase 3 and Phase 5 ledger rows the same way, including the perf probe | Phase 5 |
| 1 | T6.6 `carry-forward` | Full | opus, high | The carry-forward list and the Phase 5 QA findings | Phase 5 |
| 2 | T6.1 `accessibility` | Full, large, invariant | opus, high | AccessKit roles and focus order across the UI, keyboard context menus, keyboard access to tabs, title bar and rows | Wave 1 |
| 2 | T6.2 `visual-parity` | Full, design | sonnet, high | Headless PNG comparison, theme fixes | Wave 1 |
| 2 | T6.3 `behavior-docs` | Full, docs only, QA skipped | sonnet, medium | The behavior audit docs in the migration plan | Wave 1 |
| 2 | T6.7 `parity-fixes` | Full | opus, high | Parity bugs wave-1 QA found outside its scope | Wave 1 |
| 3 | T6.8 `final-polish` | Full | opus, high | Volume slider keyboard access, tab strip pitch, preview height | Wave 2 |

**Integrated QA, root-owned because it needs shared state:** acceptance suites,
`run-stall.sh`, `run-pause-probe.sh`, and release builds that connect to a local and an
SSH host, reported by `gdev_qa_phase6`. The owner then runs one Mac test prompt.
**The plan is done here.**

**Accepted differences from the Mac test (T6.9, dropped by the owner):**

- The window is not restored on a secondary display: GPUI's macOS bounds are relative to their display and no display is saved; on X11, GPUI writes no position hint.
- The Git rows' hover-only buttons cannot be pressed through a screen reader until the row is focused, as in Tauri.
- A `pane.reseedFailed` journal line after ⌘W: a harmless race, nothing shown.
- Red destructive menu items and the red Delete dialog button lack screen-reader names (Kit).
- As in Tauri: the shortcut field shows the stored form ("Meta+K"), terminal find searches on Enter, and Return on the New workspace host picker opens it.

## After parity

Phases 7 to 10 do not start until the owner asks for them.

## Phase 7: Live Markdown

Approved by the owner on 2026-11-11, to run through final QA.

**Scope.** Live editing with hidden syntax, as today. Inline editable tables with full
`liveTable` behaviour. Lists and list commands, task boxes, links, ⌘F find built into the
live view with a match count and replace, file sync and autosave, and IME. The Live mode
sits beside Source, Preview and Split, with the default-view setting, as in Tauri.
Images draw no picture: the alt text is styled like a link and the markup hides until the
caret enters it. Not built: spellcheck, screen-reader text, a general incremental parser,
a rope. Files above `LIVE_MARKDOWN_LIMIT_CHARS` (1 MiB) open as source, as today.

**Size (owner decision, 2026-11-12).** T7.1 measured typing smooth only to about 50 KB:
every block is laid out each frame, every keystroke reparses the whole file, and the
gpui-base 0.9.0 layout cache sweep goes quadratic past 4096 entries. T7.1 lands with a
temporary 48 KiB limit. T7.5 draws only the blocks on screen and reparses only the edited
block, then restores the 1 MiB limit. If 1 MiB is not smooth after T7.5, stop and report.

**Isolation.** Everything lives in a new crate, `crates/live-markdown`. Its modules hold
no GPUI types except one `view` module. The app registers the live view for Markdown tabs
through one seam, like `set_markdown_preview`. The view edits the editor's document
buffer, so autosave, outside-change reconciliation and dirty state stay in
`apps/desktop-gpui/src/editor`. The starting point is `spikes/gpui/markdown`.

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T7.1 `live-md-core` | Full, large, design | opus, high | New crate from the spike, editor seam, Live mode, size limit, sync, IME | |
| 2 | T7.2 `live-md-blocks` | Full | opus, high | Projection parity, list commands and indent, task toggle, link click, word motion and clicks | T7.1 |
| 2 | T7.3 `live-md-tables` | Full | opus, high | Full `liveTable` parity | T7.1 |
| 2 | T7.5 `live-md-viewport` | Full, large, design | opus, high | Draw only on-screen blocks, reparse only the edited block, restore the 1 MiB limit | T7.1 |
| 3 | T7.4 `live-md-find` | Full | opus, high | ⌘F in the live view: highlights, count, replace | T7.5 |
| 3 | T7.6 `live-md-wide-tables` | Full | opus, high | Wide tables scroll sideways and follow the caret, as in Tauri (gpui-base clips them past about 11 columns) | T7.5 |

Each task ports the Tauri tests for its area and closes those ledger rows. Root merges
wave 2 in the order T7.3, T7.2, T7.5, then runs T7.4 and T7.6 on top, because they and
T7.5 all change how the view draws; T7.6 merges before T7.4. Accepted (owner, 2026-11-12):
a space typed at the end of a table cell stays in the file; Tauri trims it when the caret
leaves the cell. Then `gdev_qa_phase7` runs on Linux, local and
SSH, and the owner runs a Mac test for typing feel and IME.

## Phase 8: Packaging and CI

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T8.1 `linux-package` | Full, invariant | sonnet, high | `release/linux/**`, `release/get.sh` | Owner approval |
| 1 | T8.2 `macos-package` | Full, invariant | sonnet, high | `release/macos/**`, new `Info.plist` | Owner approval |
| 2 | T8.3 `repo-ci` | Full, invariant | sonnet, high | Workflows, version scripts, test harness scripts, root `package.json` | T8.1, T8.2 |

**Worker QA:** T8.1 runs `verify-package.sh` and installs into a scratch prefix. T8.2 can
only be verified on a Mac that agents can reach. Without one, it stops at a CI build and
the owner verifies the package. T8.3 lints the workflows and runs the harnesses.

**Integrated QA:** `gdev_qa_phase8` installs over a Tauri install in scratch state,
confirms profiles and settings survive, and uninstalls cleanly.

## Phase 9: Cutover

Sequential and on one branch, because it is one PR by design.

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T9.1 `cutover` | Full, large, invariant | opus, high | Copies mock assets first, then deletes Tauri and React, renames the app, updates workspace and patches | Phase 8, owner approval |
| 2 | T9.2 `docs-sweep` | Full, docs only, QA skipped | sonnet, high | Every doc and code comment in the migration plan's tables, `docs/desktop-architecture.md` | T9.1 |

**Tests:** full `cargo test --workspace`, `pnpm mobile:check`, `pnpm mobile:test`, and the
final sweep command returning only justified hits.

## Phase 10: Final integrated QA and release

1. Root merges and opens the cutover PR, with the owner's approval.
2. Fresh `gdev_qa_final` runs the whole product end to end from a release candidate:
   install, connect local and SSH, terminal with a real agent, files, Git, ports,
   transfers, notifications, upgrade from Tauri, uninstall, and a mobile connection to
   the new host to confirm the protocol is unaffected.
3. Root reports concrete results. The owner runs the release.
4. Move this file, the migration plan and the ledger to `docs/history/plan/`.

## Status

| Phase | State | Commits | Next step |
|---|---|---|---|
| 0 | Done on Linux, stopped as agreed. Conditional go recommended | | Linux only, complete. Report: `docs/gpui-phase0-report.md`. Waiting for the owner's decision |
| 1 | Done. `gdev_qa_phase1` passed | `7a4f66c`, `9c13af4`, `1b7df0a`, `29ee6f3` | Known: `test:git` fails on `origin/main` too, in the driver's patch check, so T5.3 must fix or retire it. `test:lifecycle` failed 1 of 34 runs at line 320, cause not found |
| 2 | Done, QA passed | `08f8108` | X11 window position is open for the owner |
| 3 | Done, QA passed | `e5e5f77` | Carry-forward items below |
| 4 | Done, QA passed | `0912995` | |
| 5 | Done | `ec39e80` | Integrated QA passed on local and SSH hosts |
| 6 | Done, owner Mac test done | `f93e815` | Integrated QA passed. Mac test found bugs, now being fixed. Benchmark: `docs/gpui-benchmark.md`; the owner committed to GPUI |
| 7 | Done on Linux: T7.1 to T7.8 merged (1 MiB limit), `gdev_qa_phase7` PASS | | Owner Mac test |
| 8 to 10 | After parity, owner decides | | |

### Task log

Root adds a row when a task is dispatched, and updates it on completion. Commit hashes for
Phase 0 are not in the published history.

| Task | Agent | Branch | State | Commit | Notes |
|---|---|---|---|---|---|
| T1.1 `golden-paths` | sonnet, medium, full cycle | `gpui/golden-paths` | Merged | `7a4f66c` | Root fixed a shared `HOME` race by moving the checks to a child process. Passes on macOS CI |
| T1.2 `prefs-to-app-state` | sonnet, high, full cycle | `gpui/prefs-to-app-state` | Merged | `9c13af4` | Sounds import once. Theme and zoom reach first paint through an injected boot script, so the window is created in `setup`. QA passed on Linux |
| T1.3 `core-traits` | opus, high, full cycle, large, invariant | `gpui/core-traits` | Merged | `1b7df0a` | 2 review rounds, 5 GiB transfer QA passed. No dialog trait, because Rust has no dialog call. T1.4 must replace the Tauri spawn re-export in `platform.rs` |
| T1.4 `core-crate` | opus, high, full cycle, large, invariant | `gpui/core-crate` | Merged | `29ee6f3` | No `tauri` in `cargo tree`. 1 review round. Lifecycle lane passed 3 of 4 runs, cause of the one failure not found |
| T2.1 `app-foundation` | opus, high, full cycle, large, design | `gpui/app-foundation` | Merged | `08f8108` | 2 review rounds, QA passed. Vendored `gpui-pre-macos` patch. Open: title-bar drag under openbox, for T2.5 |
| T2.2 `chrome-layout` | opus, high, full cycle, design | `gpui/chrome-layout` | Merged | `fd15463` | 3 review rounds. QA found 4 bugs, fixed after QA. Adds atomic `update_app_state`. T6.1 must add keyboard access to tabs, a regression until then. T2.5 adds tooltips on disabled controls |
| T2.3 `commands-keymap` | opus, high, full cycle | `gpui/commands-keymap` | Merged | `3c6eb38` | 1 review round. QA 6 of 7, shortcut-editor bugs fixed after QA and covered by tests only. Known: digit selectors do not resolve on layouts with shifted digits such as AZERTY. T2.5 must add menu accelerators itself |
| T2.4 `connections` | opus, high, full cycle, invariant | `gpui/connections` | Merged | `a9823b4` | 2 review rounds. QA passed except Enter on a focused confirm button, fixed after QA with unit tests only. `gdev_qa_phase2` must re-check it and real quit. Status pill placement open |
| T2.5 `native-surfaces` | sonnet, high, full cycle, design | `gpui/native-surfaces` | Merged | `bee12e9` | 3 review rounds. QA passed with observations. Merge kept `notices.rs` as the one status-notice port and removed the T2.4 copy. macOS, Wayland and a real compositor not run. Saved x,y did not match in 3 of 4 runs under emulated compositor. Bar buttons not reachable by Tab, for T6.1 |
| `gdev_qa_phase2` | fresh QA | `gpui/integration` | Passed | | 14 of 15 items passed. Fixed after QA: focus lost after the helper dialog, unbounded problem notice, status pill over the host row, CI scheduler failure. Open for owner: X11 window position is not restored (size is), because GPUI sets no position hint. Context menu has no Phase 2 surface, so its real-app check waits for Phase 4 |
| T3.1 `terminal-engine` | opus, high, full cycle, large, invariant | `gpui/terminal-engine` | Merged | `5db235a` | 2 review rounds, replay QA on real tmux recordings. Changes host `seed.rs` to erase before text, no frame format change. Open: ZWJ width skews paging (tmux 2 cells, alacritty 4); host seeds `mouse_any_flag` as `?1003`, a bug that predates the port |
| T3.2 `terminal-interactions` | sonnet, high, full cycle | `gpui/terminal-interactions` | Merged | `d563c37` | 1 review round plus differential fuzz, 0 mismatches. QA skipped, pure logic. T3.5 implements its `Buffer` traits |
| T3.3 `terminal-element` | opus, high, full cycle, large, invariant, design | `gpui/terminal-element` | Merged | `a888887` | 2 review rounds, QA 5 of 5 (exact size, 20M-line flood, resize during flood). Engine wake polls on a timer. Root moved the status pill into the empty host row. Open: shrink then grow reflow differs from tmux, as in the current app. Idle blink costs about 15% CPU on software Vulkan |
| T3.4 `terminal-input` | opus, high, full cycle, invariant | `gpui/terminal-input` | Merged | `3fa49fe` | Restarted after an out-of-memory kill. 1 review round, QA 9 of 9 incl. fcitx5 XIM. Paste moves to the host path in T3.8, owner approved |
| T3.5 `terminal-reading` | opus, high, full cycle | `gpui/terminal-reading` | Merged | `bc6ce34` | Restarted after an out-of-memory kill. 2 review rounds, QA passed 6 of 6; fixes for its light-theme and stale-underline bugs not re-checked live, so `gdev_qa_phase3` covers them. Opening `~` and relative file links waits for Phase 4/5 |
| T3.6 `terminal-integration` | opus, high, full cycle, invariant | `gpui/terminal-integration` | Merged | `b9b25b3` | Restarted after an out-of-memory kill. 2 review rounds, 2 QA runs pass incl. screenshot paste over SSH. Open: about 24 MB clipboard images time out in desktop-core's reader, likely in Tauri too |
| T3.7 `seed-fixes` | sonnet, high, full cycle | `gpui/seed-fixes` | Merged | `f91e002` | 1 review round, QA pass on tmux 3.8 (old seed failed 5 of 8 cases). Also fixes Tauri and mobile seeds. tmux 3.3 to 3.7 not tested |
| T3.8 `desktop-paste` | opus, high, full cycle, invariant | `gpui/desktop-paste` | Merged | `34a57c9` | Owner chose the host paste path. QA pass on tmux 3.4 and 3.8, also after a reseed. Open: keys typed within about 200 ms of Ctrl+V go before the paste, because the clipboard read is slow; the Tauri app reads the clipboard asynchronously too |
| `gdev_qa_phase3` | fresh QA | `gpui/integration` | Passed after a fix | | Claude Code and Codex local and over SSH, resize during floods, VT parity (only known VS16/ZWJ and pause-idle differences). Found a crash on typing in a scrolled-back pane, fixed in T3.5 `71f5a3d` and re-checked 13 of 13. Root fixed the link underline stopping one cell short |
| T4.1 `workspaces-shell` | opus, high, full cycle, large, design | `gpui/workspaces-shell` | Dispatched | | |
| T4.2 `agents-automations` | opus, high, full cycle, invariant | `gpui/agents-automations` | Dispatched | | |
| T4.4 `settings-diagnostics` | sonnet, high, full cycle, design | `gpui/settings-diagnostics` | Dispatched | | |
| T0.1 `license-gate` | sonnet, medium, full cycle | `gpui/license-gate` | Merged | `c025ace`, merge `259e207` | MPL-2.0, BSL-1.0, CDLA-Permissive-2.0 and LLVM-exception crates allowed per crate. Owner to confirm |
| T0.2 `ledger` | sonnet, medium, impl only, reviewed in T0.7 | `gpui/ledger` | Merged, review pending in T0.7 | `68f33c9` | S8: no drag has to reach the OS. 402 files: 336 port, 31 replace, 35 retire |
| T0.3 `spike-scaffold` | sonnet, high, full cycle | `gpui/spike-scaffold` | Merged | `9a7bdb0` | S6 builds on 1.99.1, license clean, 1.97.1 unproven. `.ttf` required. AccessKit works. Variable Inter weights not honored. |
| T0.4 `spike-terminal` | opus, high, full cycle, large, invariant | `gpui/spike-terminal` | Merged | `0ff19eb`, merge `18c463d` | Software rendering only, no xterm number, so S1 and S2 are open. Filled 8 panes p95: key to present 21.6 ms. Flood 9.9 MB/s, memory flat. Resize and switch under flood take 1.5 to 4.9 s. Open defects: no recovery after `%pause`, TUI resized under flood stays corrupt, keys reach the wrong pane while focus lags |
| T0.5 `spike-markdown` | opus, high, full cycle, design | `gpui/spike-markdown` | Merged | `a46e831`, merge `adb8892` | S4: works on `gpui-base` `editor_host`, not the Kit code editor. Full parity estimated at 6 to 9 weeks, fallback 3 to 5 days. Large-file performance and accessibility not prototyped |
| T0.6 `vt-parity` | opus, high, full cycle | `gpui/vt-parity` | Merged | `cfa7fdb`, merge `3f9b83f` | S9: wrap passes, CJK and plain emoji pass, VS16 and ZWJ emoji fail. Lane exits 1 by design and no script gates on it. Root measured xterm: Unicode 6, also wrong on VS16 and on plain emoji |
| T0.7 `spike-ci-baseline` | sonnet, high, full cycle | `gpui/spike-ci-baseline` | Merged | `a0a9b1f` | S6 passes on 1.97.1. S7 passes locally and in an Ubuntu 24.04 container. Hosted CI and the macOS job have not run, because nothing is pushed. Ledger review moved 9 rows from retire to port and 2 to replace, and root moved `paintTailProbe` to replace after QA: 345 port, 34 replace, 23 retire |
| `gdev_qa_phase0` | fresh QA | `gpui/integration` | Passed | | All 8 checks ran. Doc errors it found were fixed. S3 and S10 run by root. S5 not proven |
