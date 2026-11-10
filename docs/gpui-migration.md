# Desktop migration to GPUI

Status: Phase 0 and the S11 host test are complete. The owner's decisions are in `docs/gpui-autonomy-decisions.md`. Phase 1 is next. The shipping app is unchanged.

Replace the Tauri, React and WebKit desktop app with a native GPUI app, at feature
parity, on Linux x86-64 and macOS Apple Silicon. The host helper, the wire
protocol and the mobile app stay as they are.

**Done means:** a fully working GPUI app with feature parity, at the end of Phase 6,
except for the [exceptions](#exceptions-to-parity). Measuring, piloting, Live Markdown,
packaging and cutover come after that, when the owner wants them.

## Decisions

| Decision | Choice |
|---|---|
| UI framework | GPUI, through GPUI Kit (`gpui-kit`, which pins a matching `gpui-pre` snapshot) |
| License | The desktop app stays MIT. No GPL or AGPL code enters it (see [License rules](#license-rules)) |
| Terminal | `alacritty_terminal` for parsing and grid state, with a terminal view written for Muxflow |
| Rollout | The new app is built beside the old one and replaces it in one cutover. The two never ship together, and no compatibility layer joins them |
| Web content | No webview in the finished app. `gpui-webview` is X11-only on Linux (it depends on `gtk` and `gdkx11`) and would keep WebKitGTK as a dependency |
| Wire protocol | No change planned (see [Protocol impact](#protocol-impact)) |
| Live Markdown | Moves to Phase 7, after parity. Built on `gpui-base` `editor_host` (spike T0.5) if the owner still needs it after the pilot. Until then, the source editor with the rendered preview |
| Simplicity | Prefer the simplest code that keeps the behavior. Do not add abstractions, options or framework layers a task does not need. Keeping the codebase small outranks schedule |

## Exceptions to parity

Keep feature parity, except when the complexity is large compared to the gain. The
owner accepted these. Each one is recorded in the ledger and can be added later.

| Item | Result |
|---|---|
| Monaco error marks and code completion | Not built. Tree-sitter highlighting stays. LSP over SSH is a separate project |
| Live Markdown spellcheck | Not built. GPUI has none |
| Live Markdown for large files | No virtualized view or incremental parser. A measurement sets the size limit, and larger files open as source |
| Screen-reader access to terminal and Live Markdown text | Not built. The terminal mirror and its setting go away. Controls keep their AccessKit roles |
| `MUXFLOW_NATIVE_DECORATIONS`, `MUXFLOW_WINDOW_CONTROLS` | Removed. Automatic title-bar selection stays |

Rune was considered and rejected for now: it is pre-release, and text editing, IME,
accessibility and virtualized scrolling are still planned work.

## What is being replaced

Measured from the repository when this plan was written.

| Area | Size | Fate |
|---|---|---|
| `apps/desktop/src` production TS/TSX | about 45k lines | Rewritten in Rust |
| `apps/desktop/src` tests | about 40k lines | Ported or retired (see [Test migration](#test-migration)) |
| `apps/desktop/src/styles.css`, `tokens.css` | about 2.2k lines | Become a Rust theme |
| `apps/desktop/src-tauri/src` | 31k lines, 71 files, 36 of which touch Tauri | Kept, moved behind a UI-free crate |
| Tauri commands | 77 `#[tauri::command]` functions, including perf-log stubs | Become direct Rust calls |
| Streaming IPC | 18 `Channel<InvokeResponseBody>`, 10 `Channel<Value>` | Become typed in-process channels |
| `crates/protocol`, `crates/tmux-control`, `crates/phase0-core`, `apps/host` | | Unchanged |
| `apps/mobile`, `packages/*` | | Unchanged, except that the palette generator gains a Rust output |

Largest production areas: `features/terminal` 11.6k, `features/files` 7.8k,
`app` 7.3k, `features/shell` 4.5k, `features/agents` 3.1k, `features/git` 2.4k,
`features/workspaces` 2.2k.

## Target architecture

```text
crates/desktop-core        Everything in src-tauri that is not UI: connections,
                           transfers, profiles, app state, notifications,
                           incidents, update check, perf-log sink. No Tauri, no GPUI.
crates/terminal-engine     alacritty_terminal per pane, the port of TerminalEventHub
                           (generations, seeds, backlog, pane health), history
                           paging. No GPUI, so it is fully unit-testable.
crates/terminal-interactions
                           Rust port of packages/terminal-interactions: links,
                           file paths, selection cleanup.
apps/desktop-gpui          The GPUI app. Renamed to apps/desktop at cutover,
                           when the binary becomes `muxflow`.
```

Terminal data path:

```text
host ─► desktop-core bridge ─► terminal-engine thread ─► grid + damage
                                     │                       │
                         host credit ack              coalesced notify
                                                             ▼
                                               TerminalElement paints rows
```

- **Parsing runs off the main thread.** Bytes never cross a serialization boundary.
  GPUI repaints at most once per frame, however much output arrived.
- **Flow control stays.** `delivery_window.rs` and `delivery_ack.rs` translate
  JavaScript's frame acknowledgements into host credit today. The engine becomes
  the acknowledger, after admission. The host credit protocol does not change.
- **Hidden panes keep their grids in memory,** which should retire the
  `addon-serialize` cache in `TerminalStateCache`.
- **State:** React hooks and `useApp*Controller` modules become GPUI entities. Keep
  module names close to their TypeScript originals so review can compare them.
- **Async bridge:** `desktop-core` owns its tokio runtime. UI entities receive
  results over channels polled with `cx.spawn`.

## License rules

These are mandatory for every phase, for people and agents.

1. **Do not read, port or paraphrase** Zed's `editor`, `terminal`, `terminal_view`
   or `workspace` crates, or any other GPL or AGPL source. Work from the APIs of
   `gpui`, `gpui-kit` and `alacritty_terminal`, and from Alacritty's own code.
2. **`cargo deny check licenses` gates CI from Phase 0.** The allowlist is permissive
   licenses only. GPL, AGPL and unknown licenses fail the build.
3. **Ship notices.** Apache-2.0 dependencies are fine in an MIT app, but releases
   must carry their license texts and any NOTICE files. Generate these in the
   package build.
4. **Other GPUI terminals:**

| Project | License | Rule |
|---|---|---|
| `gpui-terminal` | MIT OR Apache-2.0 | Reference only, after checking it does not derive from Zed's GPL code |
| `lassejlv/termy` | MIT | Same as above |
| `iamazy/termua` | AGPL-3.0 | Do not read or copy |
| `douo/rterminal` | No clear license | Do not read or copy |

Checked when this plan was written: `gpui-pre`, `gpui-kit`, `gpui-webview`,
`gpui-fast` and `alacritty_terminal` are Apache-2.0.

## Protocol impact

No wire change is planned, so both directions required by `CLAUDE.md` are unaffected:

- The previous released mobile app against the new host: no change, because the
  host is untouched.
- The new mobile app against the previous released host: no change, because mobile
  is untouched.

`PROTOCOL_MAJOR` is not bumped. If any phase finds it needs a wire change, the host
and desktop change in the same commit, the old path is deleted, and both directions
are answered in that review.

The host needs no logic change, but it was written with xterm as the consumer, so two
contracts must hold in the new engine:

- History pages rely on autowrap. `composeHistoryPage` writes a full-width row with no
  line break so the continuation is marked as wrapped (see `service/terminal.rs` near
  line 1707). `alacritty_terminal` has the equivalent `WRAPLINE` flag, and S9 proves it.
- The desktop must keep sending the on-screen palette as `TerminalColors`, because a
  control-mode client has no TTY to report colors from (`service/terminal.rs` near
  line 502).

## Replacement map

| Today | Replacement | Notes |
|---|---|---|
| Tauri app, commands, events | GPUI app calling `desktop-core` | |
| xterm.js, WebGL addon, patches | `alacritty_terminal` and `TerminalElement` | The atlas patches and probes go away |
| `addon-search` | `alacritty_terminal` regex search | |
| Monaco editor | Kit code editor (Tree-sitter) | Error marks and code completion are not carried over |
| Monaco diff editor | Kit `Diff`, read-only and virtualized | It takes a patch, so generate one from both texts with `similar`. Today's view is read-only, side by side and wrapped |
| CodeMirror Live Markdown | Kit code editor with `TextView` preview in Phase 5. Custom editor on `gpui-base` `editor_host` in Phase 7, if built | Kit decorations are fills and frames. See `spikes/gpui/markdown/REPORT.md` |
| `@muxflow/markdown` (marked, DOMPurify) | Kit native Markdown rendering | No HTML execution, so the sanitizer and CSP have no equivalent to keep |
| `tauri-plugin-dialog` | GPUI path prompts | Depends on the XDG portal on Linux; verify under Hyprland |
| `AudioContext` agent sounds | `rodio` | Adds ALSA as a Linux dependency |
| `menu.rs` | GPUI `set_menus` | Native on macOS, in-app on Linux |
| `linux_window.rs` | GPUI client decorations | Keep automatic selection. Remove `MUXFLOW_NATIVE_DECORATIONS` and `MUXFLOW_WINDOW_CONTROLS` |
| `macos_window.rs` | Port to GPUI's window handle | Full-screen behaviour, traffic lights at (12, 13) |
| `clipboard-rs`, `wl-clipboard-rs`, `NSPasteboard` | Kept | Native paste already lives in Rust |
| `notifications/*`, `power_events.rs`, `update_check.rs` | Kept | Replace only the Tauri event emission |
| `packages/client-core` palette | Generator also emits Rust | One source of truth for desktop and mobile |
| Fonts (`.woff2`) | TTF or OTF, embedded | `apps/mobile/assets/fonts` may already have what is needed |

## Phases

Each phase is one or more PRs, and only the exit gate ends it. Exit gates in Phases 1
to 6 check that features work. They do not check performance numbers.

Work in a worktree at `../muxflow-gpui`, on branches under `gpui/`.

### Phase 0: Feasibility spike, go or no-go

A failed gate stops the migration, or sends a decision back to the owner.

Phase 0 is scoped to Linux (Hyprland and X11). The macOS parts of S3, S5 and S10 are
deferred and must pass before Phase 2 starts, because finding a macOS blocker later would
waste more work.

| Spike | Question | Gate |
|---|---|---|
| S1 Terminal latency | One pane fed by `crates/tmux-control` through `alacritty_terminal` | p95 key-to-glyph no worse than xterm on the same machine, measured with the `echoLagProbe` method |
| S2 Flood and layout | Eight visible panes under flood, plus resize and tab switch | Typing during flood and resize no worse than xterm, with no unbounded memory growth |
| S3 Platforms | Hyprland, GNOME Wayland, X11, macOS 14 | Client decorations, HiDPI, file drop in, clipboard and path prompts all work |
| S4 Live Markdown | Hidden syntax and an editable inline table on the Kit editor or `gpui-base` text | Works, or decision 3 is made |
| S5 Text input | IME composition in a terminal and in the editor on Wayland and macOS | CJK composition works, with the candidate window placed at the caret |
| S6 Toolchain | Builds on Rust 1.97.1 with exact pins; `cargo deny` passes | Clean build and clean license report |
| S7 CI rendering | Headless GPUI tests and pixel captures on Linux CI, using lavapipe if needed | Both run in CI |
| S8 Drag out | Do any drags have to reach the OS? | Answered: no. No data drag leaves the app: all drag sources are in-app, and only inbound file drop is OS-facing. The title-bar window drag (`data-tauri-drag-region`) moves the window and belongs to S3 client decorations (see the ledger) |
| S9 VT parity | Wide characters, emoji, combining marks and history-page autowrap against tmux | No grid disagreement in a `run-vt-parity.sh` lane that uses the new engine, and wrapped history reflows and copies like live output |
| S10 Test driver | Packaged journeys and `measure-ui-latency.sh` read the app through the accessibility tree, which WebKit provides for free and GPUI does not | `cua-driver` can find, read and press an element in the spike through AccessKit |

Also in this phase:

- Add `cargo deny` to CI.
- Create `docs/gpui-migration-ledger.md`, generated from `git ls-files apps/desktop/src`.
  Every module and test file gets a status of port, replace or retire, and a retire
  needs a one-line reason.

Exit: a written go or no-go with the measurements, and decisions 1 to 4 recorded.

### Phase 1: Extract `desktop-core`, still shipping Tauri

1. Record the paths Tauri resolves (`app_config_dir`, `app_log_dir`, `app_data_dir`)
   on both platforms as golden tests. The native app must use the same directories,
   so profiles, app state and logs carry over.
2. Introduce `EventSink` and `AppPaths` in place of `Channel`, `emit`, `AppHandle` and
   `State`. Rust makes no dialog calls today, so a dialog trait waits until Phase 2 needs one.
3. Move `connection/**`, `app_state`, `incidents`, `notifications`, `update_check`,
   `power_events`, `external_links` and the perf-log sink into `crates/desktop-core`.
   Tauri commands stay as thin shims until cutover.
4. Done: `muxflow.theme`, `muxflow.uiScale` and `muxflow.agent-sounds.v1` no longer
   live in `localStorage`. Ship that release before cutover, because WebKit storage is
   not readable from the native app afterwards.
5. Repoint scripts that name moved code or the `muxflow` package, including the
   `cargo test -p muxflow` fixtures in `tests/performance/optimization/run-before.sh`
   and the file list in `tests/integration/transfers/source-tree-evidence.sh`.

Exit: `cargo tree -p desktop-core` contains no `tauri`, all existing tests pass, and
the app behaves the same.

### Phase 2: Native shell

- `apps/desktop-gpui` with window, title bar, minimum size 320×240, theme tokens,
  UI scale and fonts.
- Dock layout, sidebar, tab strip and right panel, using Kit `dock`, `tab`, `sidebar`.
- The command registry (64 commands), default keymaps for macOS and Linux, shortcut
  overrides from `app_state`, command palette, Quick Open, and dialogs.
- Connection, host profile and setup flows through `desktop-core`, including
  missing-helper recovery.
- Notices, context menus, tooltips, app menu, and window geometry restore.

Exit: the app connects to local and SSH hosts, restores saved tabs as placeholders,
and every command appears in the palette.

### Phase 3: Terminal, the highest-risk phase

Port the state machines first, in `terminal-engine`, along with their tests:
`TerminalEventHub`, `PaneHistoryPager`, `historyPage`, `TerminalGenerationWatermark`,
`TerminalReadingState`, `PaneDegradedWatchdog`, `PaneRecovery`, `PaneRevealState`,
`revealRetry`, `DeferredTerminalOutputQueue`.

Then build the view:

- **Rendering:** cell metrics, row shaping with damage-based caching, cursor styles,
  bold, dim, italic, underline and undercurl, box drawing, theme and host colors.
- **Sizing:** `clientSize`, `useClientResize` and `terminalViewport` must keep tmux
  client size exact.
- **Input:** `terminalInputPolicy`, the explicit `0x1f` for Ctrl+/, bracketed paste,
  mouse reporting, kitty keyboard, IME, and the Linux and macOS terminal key conflicts
  listed in `docs/keybindings.md`.
- **Reading:** selection, wrapped-command copy cleanup, scrollback, tmux history
  paging, search, web links and file links including `~` paths.
- **Integration:** OSC 52, screenshot and file paste, drag and drop, the transfer
  surface, `paneRouting`, pane moves, hover focus, agent state, and the `TerminalColors`
  report to the host.
- **Conformance:** shared JSON fixtures used by both `cargo test`
  (`crates/terminal-interactions`) and vitest (`packages/terminal-interactions`), so
  desktop and mobile do not drift apart.

Retired, per the ledger: `TerminalStateCache` (its `screenSeeded` and top-of-history
flags become plain pane state), `atlasFontSmoothing`, `atlasStaleProbe`, and
`tests/performance/runtime/atlas-merge-repro`.

Replaced, with contracts that must survive:

- `TerminalRenderer`: `restoreDecision`, `activatedTerminalUrl`, grid reconciliation and
  resource recovery.
- `TerminalWriteScheduler`: the bounded queue, overflow-to-reseed and the rendered
  checkpoint.
- `panePaintGate`: the Explorer root probe still waits for the pane's first paint, so a
  slow link carries the terminal first.

Exit:

- Claude Code and Codex run correctly in the terminal, locally and over SSH.
- VT parity passes at all five quiesce points.
- Resizing during heavy output repaints correctly.

### Phase 4: Workspaces and panels

Port `features/workspaces`, `features/shell`, `features/tmux`, `features/agents`
(including sounds), `features/automations`, `features/ports`, `features/transfers`,
`features/dragDrop`, Settings, `src/diagnostics` and `src/startup`. Voice is
mobile-only today, so it is not part of this phase.

Exit: every panel works against a real host, and the ledger shows these areas done.

### Phase 5: Files, editor, Git

- **Explorer:** port the tree, its window and cache, watch leases, mutation dialogs,
  file icons and gitignore toggle.
- **Editor:** tabs and preview tabs, autosave, remote save conflicts, search with
  counts, Tree-sitter grammars with their licenses checked. Port the Python and
  TypeScript additions from `monarch.ts` as highlight queries.
- **Markdown:** the source editor with the rendered preview. Live Markdown is Phase 7.
- **Git:** `features/git` stage, commit and push flows, and the diff view with wrapped,
  aligned columns.

Exit: a file can be browsed, edited, diffed, staged, committed and pushed on a remote
host, and the Git diff stays aligned at a 320 px panel width.

### Phase 6: Parity check

- Accessibility: AccessKit roles on controls, and focus order.
- Visual comparison against the Tauri app, using headless PNG captures in both themes.
- Rerun `tests/acceptance`, `run-stall.sh` and `run-pause-probe.sh`.
- Build release binaries on Linux and macOS, and connect each to a local and an SSH host.
- The owner runs one Mac test prompt, including IME input.

Exit: the ledger has no open items, these checks pass, and the app runs on both
platforms. **The plan is done here.**

## After parity

The owner decides if and when each step starts. Tauri stays in place until cutover.

### Phase 7: Live Markdown

The owner decides after the pilot if this phase runs. If it runs, it must finish before
cutover, because cutover removes the current Live Markdown.

Build on `gpui-base` `editor_host`, starting from `spikes/gpui/markdown`, using the work
list in its `REPORT.md`. Do not build spellcheck, a virtualized view, an incremental
parser or screen-reader text. Measure the largest file that stays smooth, and open
larger files in the source editor.

Exit: Live Markdown matches today's behavior for files below the limit, including
hidden syntax, inline tables, list commands, file sync and IME.

### Phase 8: Packaging and CI, prepared on the branch

**Linux**
- `release/linux/build-package.sh`: drop the Vite and Tauri embedding steps and
  `TAURI_CONFIG`.
- `release/get.sh`: replace the WebKitGTK probe (lines 185 to 197) with the new
  runtime check: Vulkan loader, xkbcommon, and anything else `ldd` reports.
- `release/linux/verify-package.sh`, `release/linux/install.sh`: update for the new
  dependencies.
- Re-evaluate the ARM64 blocker, since it existed because of the WebKitGTK sysroot.

**macOS**
- `release/macos/build-package.sh`: replace `tauri build` with `cargo build` and a
  checked-in `Info.plist`, then assemble the `.app`, including `muxflow-host` and
  `package-owner` where `connection/helper.rs` expects them.
- Keep `dev.muxflow.desktop` as the bundle identifier, so notification permission and
  the `owner` check in `uninstall.sh` continue to work.
- Re-verify signing, notarization, `stapler.sh` and `verify-package.sh`.

**Repository**
- `release/check-version.sh`, `release/set-version.sh`: remove `tauri.conf.json` and
  `apps/desktop/package.json`.
- `.github/workflows/ci.yml`, `release.yml`, `.github/scripts/apt-install.sh` callers:
  remove `TAURI_CONFIG`, WebKitGTK packages and the desktop `pnpm` steps, and add the
  GPUI build dependencies. Keep `mobile:check` and `mobile:test`.
- `tests/release/run-regressions.sh` (`tauri-release` gate),
  `tests/integration/lifecycle/run-thin-desktop.sh`,
  `tests/performance/runtime/setup-qa.sh`, `tests/release/run-scale-ssh.sh`.
- `tests/integration/shell/run-shell.sh`, `tests/integration/transport/run-local.sh`
  (both run `pnpm check` and `pnpm build`), `tests/performance/runtime/run-perf.sh`,
  `tests/release/setup-cua.sh`, `tests/performance/optimization/summarize-baseline.py`.
- Root `package.json`: remove `dev`, `build`, `check`, `test` and `tauri`, or point them
  at Cargo equivalents.
- Root `Cargo.toml`: workspace members and the `tauri dev` comment. `.gitignore`:
  the `src-tauri` entries.

Exit: signed macOS and Linux packages install, upgrade over a Tauri install, keep
profiles and settings, and uninstall cleanly.

### Phase 9: Cutover

In a single PR:

1. Delete `apps/desktop/src`, `apps/desktop/src-tauri`, `index.html`, `vite.config.ts`,
   `tsconfig*`, `package.json`, `public`, and the Tauri command shims.
2. Move `apps/desktop-gpui` to `apps/desktop`.
3. Remove `apps/desktop` from `pnpm-workspace.yaml`, and remove the
   `@codemirror/view` and `@xterm/addon-webgl` patches. Keep the `@xterm/xterm` patch
   only if mobile still needs it, since mobile depends on that package.
4. Refresh the lockfiles.
5. Apply the [documentation updates](#documentation-updates).

Exit: `main` builds with no Tauri, React, xterm-for-desktop, Monaco or CodeMirror
dependency, and the [documentation sweep](#final-sweep) is clean.

Rollback: revert the cutover PR and ship the last Tauri release. On-disk state is
unchanged in format, so this is safe.

### Phase 10: Release and close-out

- Release candidate through `pnpm test:release:heavy`, then a release with notes in
  `release/release-notes.md` covering the new GPU requirement on Linux.
- Move this plan and its ledger to `docs/history/plan/`.
- Kill stale dev daemons, per `CLAUDE.md`.

## Measuring after parity

These numbers do not block the plan. Use them during the pilot.

`tests/performance/runtime/budget-report.sh` budgets continue to apply. They measure
the host and link, and have no desktop in them.

| p95 | Local | Shaped 100 ms SSH |
|---|---|---|
| Keystroke to glyph echo | 35 ms | 135 ms |
| Window switch | 100 ms | 150 ms |
| New tab interactive | 300 ms | 500 ms |
| Resize settle | 100 ms | 150 ms |
| Typing during flood | 35 ms | 350 ms |
| App overhead over raw ssh and tmux | 10 ms | 10 ms |

The native app adds gates in the renderer, which that report lists as not measured:

- Capture a baseline from the Tauri app before cutover using the existing probes.
- Key-to-glyph, window switch and flood must be no worse than that baseline.
- Explorer: 4,096 entries in 150 ms or less, and external changes in 300 ms or less.
- Idle CPU and memory with a fixed set of panes must be recorded, and should improve.

Keep the perf-log schema and `summarize-perf-log.py`, so old and new runs stay
comparable. Move `perf-log-contract.json` with the app.

## Test migration

Port logic and state-machine tests. A small end-to-end set replaces the view tests.

| Test type | Treatment |
|---|---|
| Rust tests in `src-tauri` | Move with their code |
| Pure TS logic tests (hub, pager, fuzzy, registry, listing models, autosave) | Port to Rust table tests. These protect the most behavior |
| Component tests (`*.test.tsx`) | Port where they assert behavior, using GPUI's test context and Kit's headless windows |
| Tests of React, xterm or Monaco mechanics | Retire, with a reason in the ledger |
| `tests/integration`, `tests/performance` protocol lanes | Unchanged, since they drive the host |
| `tests/acceptance`, `tests/release` | Update how the app is built and launched |
| Packaged `cua` journeys | Keep. They depend on AccessKit roles, so build those from Phase 2 rather than leaving them to Phase 6 |

Port tests alongside the code in the same PR, not afterwards.

## Documentation updates

Line numbers are from when this plan was written. Docs change in the phase that
changes the behavior they describe, and the sweep runs at cutover.

### Must change

| File | Change | Phase |
|---|---|---|
| `README.md` | Build from source (177 to 184): remove Tauri prerequisites and `pnpm tauri dev`, add the GPUI build deps and `cargo run`. Re-shoot screenshots, GIFs and the demo cover | 8 |
| `docs/setup.md` | Linux requirements (5), toolchain and packages (18 to 20), remove `TAURI_CONFIG` (25, 26), new check, test and dev commands (27 to 32) | 8 |
| `docs/troubleshooting.md` | Rewrite the xterm.js Ctrl+/ note (74, 75) and the `pnpm tauri dev` / `tauri build` note (129, 130). Add Vulkan and GPU failures | 7, 8 |
| `docs/release.md` | ARM64 sysroot (180), Vite and Tauri embedding (189, 190), WebKit entitlement reasoning (244). Add the new macOS assembly | 7 |
| `docs/third-party.md` | New font paths (4, 9), xterm patch note (11) reduced to mobile, and Apache-2.0 notices for GPUI and Alacritty | 8 |
| `docs/port-forwarding.md` | "React + Tauri" (5), Rust module path (42). The Vite mentions at 32 and 63 are example ports, so leave them | 8 |
| `docs/multi-host.md` | "React + Tauri" (5) | 8 |
| `docs/automations.md` | Paths and instructions at 447, 465, 541, 547, 583, 589, and `pnpm tauri dev` at 644 | 8 |
| `docs/mobile/toolchain.md` | Desktop font path (97) | 8 |
| `site/index.html` | Palette comment pointing at `tokens.css` (25) | 8 |
| `tests/README.md` | Say where desktop UI tests live and how they run | 8 |
| `CLAUDE.md`, `AGENTS.md` | Add the no-GPL rule and the `gpui-kit` pin policy. Keep the two files identical | 0 |
| `release/release-notes.md` | Note the migration and GPU requirement | 9 |

### Code comments and test docs

These describe the old desktop in code that otherwise stays. Reword them at cutover.

| File | Line |
|---|---|
| `apps/host/src/service/git/diff.rs` | 14, Monaco |
| `apps/host/src/service/terminal.rs` | 505, 1707, 1708, xterm and `composeHistoryPage` |
| `apps/host/src/service/terminal_tests.rs` | 1660, xterm |
| `crates/protocol/src/lib.rs` | 21, Tauri bridge |
| `crates/protocol/tests/README.md` | 18, xterm harness |

### New

| File | Content | Phase |
|---|---|---|
| `docs/desktop-architecture.md` | Crate map, thread model, data path, flow control, how to bump GPUI, license rules | 8 |
| `docs/gpui-migration-ledger.md` | Module-by-module status. Moves to history at close-out | 0 |

### Behavior audit

These name no replaced technology, but what they describe must be re-verified.

| File | Verify | Phase |
|---|---|---|
| `docs/keybindings.md` | Every shortcut and mouse gesture, on both platforms | 6 |
| `docs/remote-host.md`, `docs/mac-ssh-hosts.md` | SSH flows and helper installation | 6 |
| `docs/agent-hooks.md`, `docs/destructive-actions.md` | Agent status and confirmations | 6 |
| `docs/diagnostics-privacy-security.md` | What is logged, and that removing the webview changes nothing it promises | 6 |
| `docs/uninstall.md` | Data directories, and whether WebKit caches left by older installs need removing | 7 |

### Do not rewrite

- `docs/history/**` is a frozen record.
- `docs/mobile/design.md`, `decisions.md`, `ios-plan.md`, `voice-mode-plan.md`,
  `testflight.md`: their React, WebView and xterm mentions describe the mobile app, or
  are dated decisions. Lines such as `decisions.md:216` ("Same as the desktop") stay
  as history.
- `docs/release-readiness.md` is a local, gitignored file.

Also move `docs/design/explorer-gitignore-plan.md` to `docs/history/plan/`, because it
gives instructions against files that will no longer exist.

The mocks `docs/design/explorer-gitignore-mock.html` and
`docs/design/quick-open-mock.html` load `tokens.css` and fonts from `apps/desktop/src`.
Copy those assets into `docs/design/assets/` and repoint the links before cutover
deletes them. The sample file paths inside the mocks are illustrative, so leave them.

### Final sweep

Run at cutover. Any hit must be fixed or explicitly justified in the PR. The filter
drops `TERM=xterm-256color` and `-webkit-` CSS prefixes, which are not desktop
references.

```sh
git grep -nEi 'tauri|webkit|xterm|monaco|codemirror|src-tauri|apps/desktop/src|\breact\b|pnpm (dev|build|check)\b' \
  -- ':!docs/history' ':!docs/mobile' ':!apps/mobile' ':!packages' ':!tests/mobile' \
     ':!pnpm-lock.yaml' ':!patches' ':!docs/gpui-migration*.md' \
  | grep -vE 'xterm-256color|-webkit-'
```

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| GPUI and Kit API churn | Repeated rework | Exact pins, one PR per bump, a thin wrapper around the kit surface we use most |
| Upstream goes away or diverges | Stuck on a snapshot | Everything is Apache-2.0, so a fork is possible; keep the pin reproducible |
| Terminal behavior differs from xterm | Subtle agent TUI bugs | VT parity lane, daily use. S9 result: `alacritty_terminal` matches tmux except on VS16 and ZWJ emoji. xterm as configured today (Unicode 6) is wrong on VS16 and the family ZWJ too, and on plain emoji where the engine is right, but it is right on `👩‍💻` and `🏳️‍🌈`, where the engine is too wide. There the host's seed path erases text after the emoji, so T3.1 must stop that, by clearing the row before painting it or otherwise. No fork of the engine for now |
| Terminal behavior under flood | Frozen panes, corrupt TUIs, keys sent to the wrong pane | T0.4 found panes stay frozen after `%pause` until capture recovery is ported, which T3.1 owns. A TUI resized during flood stayed corrupt, cause unknown. Keys reached the wrong pane while focus lagged. Each needs a test in Phase 3 |
| Idle pause recovery | A paused pane may not recover when nothing else prints | The lane reports it (`vt-parity-pause-idle`). Host or tmux behavior, cause not isolated, so it needs its own investigation |
| Linux GPU requirement | Fails in VMs and on old GPUs that WebKit handled | Clear startup error, check in `get.sh`, troubleshooting entry |
| Live Markdown needs a custom editor | Large custom work | Moved to Phase 7, after parity. The owner decides after the pilot |
| Monaco feature loss | Editing feels weaker | Accepted by the owner. See [exceptions](#exceptions-to-parity) |
| Accessibility regresses | Screen readers lose ground | Kit's AccessKit support, Phase 6 audit |
| Parallel feature work on Tauri | The target keeps moving | Freeze new desktop features from Phase 2 to cutover, except fixes. Log every Tauri change in the ledger |
| GPL code enters by accident | Forces a relicense | `cargo deny` in CI and rule 1. Dependency checks do not catch copied code, so review for it |
| Scale of the rewrite | Long period with no user-visible gain | Phase 0 can stop it. Each phase ends at a checked gate |

## Owner decisions

Recorded in `docs/gpui-autonomy-decisions.md`.

