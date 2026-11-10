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

- At most **3 agents building Rust at once**, each with `CARGO_BUILD_JOBS=5`.
- Each worktree has its own `target`. Root deletes it after merging.
- A parallel task's QA must be headless and isolated: its own tmux socket
  (`tmux -L muxflow-qa-<task>`) and its own `XDG_RUNTIME_DIR`. Workers must not touch
  a host daemon or tmux server that the task did not start.
- Kill any dev daemon a task started before it finishes.
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

T3.3 defines the pane entity and the three submodule seams, so wave 3 stays apart.

| Task | Acceptance | Lightweight tests | Worker QA |
|---|---|---|---|
| T3.1 | Generations, seeds, backlog and health match the TS behavior | Ported hub and pager tests | Replay recorded tmux fixtures |
| T3.2 | Shared fixtures pass in both Rust and vitest | `cargo test`, `pnpm mobile:test` | None: pure logic |
| T3.3 | Exact tmux client size, damage-based repaint | Cell metric and row cache tests | Headless flood on an isolated socket, frame captures |
| T3.4 | `terminalInputPolicy` ported, Ctrl+/ sends `0x1f`, mouse and bracketed paste work | Table tests from the TS suite | Scripted input into a real TUI on an isolated socket |
| T3.5 | Copy cleanup, `~` links and history paging match today | Ported selection and paging tests | Scripted selection and search against tmux |
| T3.6 | Screenshot paste works over SSH, and `TerminalColors` is sent | Unit tests per surface | Docker SSH paste with an isolated socket |

**Integrated QA:** `gdev_qa_phase3` runs Claude Code and Codex locally and over SSH, the
VT parity lane at all five quiesce points, and resizing during heavy output.

## Phase 4: Workspaces and panels

All run in one wave, at most 3 at a time. Each owns its feature module and registers
through the T2.1 seam.

| Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|
| T4.1 `workspaces-shell` | Full, large, design | opus, high | `features/workspaces`, `shell`, `tmux` | Phase 3 |
| T4.2 `agents-automations` | Full, invariant | opus, high | `features/agents`, `automations`, sounds | Phase 3 |
| T4.3 `ports-transfers` | Full | sonnet, high | `features/ports`, `transfers`, `dragDrop` | Phase 3 |
| T4.4 `settings-diagnostics` | Full, design | sonnet, high | Settings, `diagnostics`, `startup` | Phase 3 |

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
| 1 | T6.1 `accessibility` | Full, large, invariant | opus, high | AccessKit roles and focus order across the UI | Phase 5 |
| 1 | T6.2 `visual-parity` | Full, design | sonnet, high | Headless PNG comparison, theme fixes | Phase 5 |
| 1 | T6.3 `behavior-docs` | Full, docs only, QA skipped | sonnet, medium | The behavior audit docs in the migration plan | Phase 5 |

**Integrated QA, root-owned because it needs shared state:** acceptance suites,
`run-stall.sh`, `run-pause-probe.sh`, and release builds that connect to a local and an
SSH host, reported by `gdev_qa_phase6`. The owner then runs one Mac test prompt.
**The plan is done here.**

## After parity

Phases 7 to 10 do not start until the owner asks for them.

## Phase 7: Live Markdown

| Wave | Task | Mode | Model, effort | Touches | Depends on |
|---|---|---|---|---|---|
| 1 | T7.1 `live-markdown` | Full, large, design | opus, high | Live Markdown on `gpui-base` `editor_host`, per the plan | Pilot, owner approval |

**Worker QA:** edit real Markdown files, including tables and lists, and test IME.

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
| 2 | Running, wave 2 dispatched | `08f8108` | T2.2, T2.3, T2.4, then T2.5 |
| 3 to 6 | Approved | | |
| 7 to 10 | After parity, owner decides | | |

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
| T2.2 `chrome-layout` | opus, high, full cycle, design | `gpui/chrome-layout` | Dispatched | | |
| T2.3 `commands-keymap` | opus, high, full cycle | `gpui/commands-keymap` | Dispatched | | |
| T2.4 `connections` | opus, high, full cycle, invariant | `gpui/connections` | Dispatched | | |
| T0.1 `license-gate` | sonnet, medium, full cycle | `gpui/license-gate` | Merged | `c025ace`, merge `259e207` | MPL-2.0, BSL-1.0, CDLA-Permissive-2.0 and LLVM-exception crates allowed per crate. Owner to confirm |
| T0.2 `ledger` | sonnet, medium, impl only, reviewed in T0.7 | `gpui/ledger` | Merged, review pending in T0.7 | `68f33c9` | S8: no drag has to reach the OS. 402 files: 336 port, 31 replace, 35 retire |
| T0.3 `spike-scaffold` | sonnet, high, full cycle | `gpui/spike-scaffold` | Merged | `9a7bdb0` | S6 builds on 1.99.1, license clean, 1.97.1 unproven. `.ttf` required. AccessKit works. Variable Inter weights not honored. |
| T0.4 `spike-terminal` | opus, high, full cycle, large, invariant | `gpui/spike-terminal` | Merged | `0ff19eb`, merge `18c463d` | Software rendering only, no xterm number, so S1 and S2 are open. Filled 8 panes p95: key to present 21.6 ms. Flood 9.9 MB/s, memory flat. Resize and switch under flood take 1.5 to 4.9 s. Open defects: no recovery after `%pause`, TUI resized under flood stays corrupt, keys reach the wrong pane while focus lags |
| T0.5 `spike-markdown` | opus, high, full cycle, design | `gpui/spike-markdown` | Merged | `a46e831`, merge `adb8892` | S4: works on `gpui-base` `editor_host`, not the Kit code editor. Full parity estimated at 6 to 9 weeks, fallback 3 to 5 days. Large-file performance and accessibility not prototyped |
| T0.6 `vt-parity` | opus, high, full cycle | `gpui/vt-parity` | Merged | `cfa7fdb`, merge `3f9b83f` | S9: wrap passes, CJK and plain emoji pass, VS16 and ZWJ emoji fail. Lane exits 1 by design and no script gates on it. Root measured xterm: Unicode 6, also wrong on VS16 and on plain emoji |
| T0.7 `spike-ci-baseline` | sonnet, high, full cycle | `gpui/spike-ci-baseline` | Merged | `a0a9b1f` | S6 passes on 1.97.1. S7 passes locally and in an Ubuntu 24.04 container. Hosted CI and the macOS job have not run, because nothing is pushed. Ledger review moved 9 rows from retire to port and 2 to replace, and root moved `paintTailProbe` to replace after QA: 345 port, 34 replace, 23 retire |
| `gdev_qa_phase0` | fresh QA | `gpui/integration` | Passed | | All 8 checks ran. Doc errors it found were fixed. S3 and S10 run by root. S5 not proven |
