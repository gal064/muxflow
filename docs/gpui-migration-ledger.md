# GPUI migration ledger

Module-by-module tracker for [the desktop migration](gpui-migration.md). Phase 0
task T0.2. One row for every tracked `.ts`, `.tsx` and `.css` file under
`apps/desktop/src` (`git ls-files apps/desktop/src`), production and tests.
`tests/release/check-migration-ledger.sh` fails if a row is missing or extra, or if a
`retire` row has no reason.

Dispositions:

- **port**: the behavior is carried over into a Rust module that mirrors this file.
  For a test, the assertions are ported.
- **replace**: the behavior stays, but a different mechanism supplies it (Kit, GPUI,
  `desktop-core` direct calls, a generated theme). Nothing is ported line for line.
- **retire**: the behavior goes away with the technology. The target column holds the
  one-line reason.

A target beginning with `?` is an open question; the file is marked `port` until it is
answered. Phase is the phase in which the row is done (a retire row, the phase from
which it is no longer needed; files are deleted together at cutover). Status is `open`
for every row until its phase closes.

Tests: port logic and state-machine tests. A small end-to-end set replaces view tests,
so a task may change a view test's row to retire, with a reason.

## Totals

| Scope | port | replace | retire | Total |
|---|---|---|---|---|
| All | 343 files / 74,428 lines | 34 files / 9,835 lines | 26 files / 2,521 lines | 403 files / 86,784 lines |
| Production | 177 files / 36,198 lines | 34 files / 9,835 lines | 14 files / 1,029 lines | 225 files / 47,062 lines |
| Tests | 166 files / 38,230 lines | 0 files / 0 lines | 12 files / 1,492 lines | 178 files / 39,722 lines |

By target phase:

| Target phase | port | replace | retire | Total files |
|---|---|---|---|---|
| 2 | 89 | 10 | 5 | 104 |
| 3 | 88 | 11 | 11 | 110 |
| 4 | 91 | 5 | 0 | 96 |
| 5 | 75 | 8 | 8 | 91 |
| 8 | 0 | 0 | 1 | 1 |

Test files marked `port` are a mix. Where a file has retire-worthy cases (React render
mechanics, xterm or Monaco internals) the target column says which cases drop.

`packages/terminal-interactions` is shared with the mobile app, so its modules sit
outside the file table. T3.2 ported its pure logic to `crates/terminal-interactions`
(done): `terminalWebLinks` to `web_links.rs`, `terminalFilePaths` to `file_paths.rs`,
`terminalLinks` to `links.rs`, `terminalSelection` to `selection.rs` and
`topologyNotification` to `topology.rs`. Both suites run the JSON cases in
`packages/terminal-interactions/fixtures`. The TypeScript package stays for mobile.

## S8: drag out

Question from the plan: the explorer, git sidebar and dock use `draggable` today;
confirm each is an in-app drag and list any that must reach the OS.

**Answer: no data drag in `apps/desktop/src` has to reach the OS. Every drag source
that carries data is in-app. Nothing needs an OS drag source from GPUI.** One drag does
reach the OS, but it carries no data: the title bar moves the window (source 5). I did not check what GPUI offers
for OS drag sources or drops, so nothing below depends on it. The one OS-facing
feature is the opposite direction (files dragged into the app), which is a drop target.

Searched `draggable`, `startDragging`, `dataTransfer`, `onDragStart`, `setData`,
`effectAllowed`, `onDragDropEvent`, `setPointerCapture`, `data-tauri-drag-region`, plus
`features/dragDrop`, `internalPathDrag.ts` and `nativeDragDropCoordinator.ts`.
`startDragging` has no hit in the source, but Tauri calls it for every
`data-tauri-drag-region` (and the capability file grants it), see source 5.

### Drag sources

| # | Source | Where | Payload | Class |
|---|---|---|---|---|
| 1 | Explorer row, HTML5 `draggable` | `features/files/ExplorerRow.tsx:51`, `:64-65`; handler `features/files/ExplorerTree.tsx:281-285`; enabled at `:434` | One MIME type, `application/x-muxflow-internal-path+json`, written at `features/terminal/internalPathDrag.ts:61-68` | In-app |
| 2 | Git sidebar change row, HTML5 `draggable` | `features/git/GitSidebar.tsx:216-221`, `:331-333` | Same writer, same MIME | In-app |
| 3 | Tab, pane or agent row, pointer capture (no HTML5 drag) | `features/dragDrop/pointerDrag.ts:147` (`beginPointerDrag`, capture at `:180`); started from `features/workspaces/TabStrip.tsx:340`, `features/workspaces/WorkspaceSidebar.tsx:337`, `app/TerminalWorkspaceSurface.tsx:109` | In-process `DragPayload` | In-app |
| 4 | Resize handles, pointer capture: dock divider, right panel width, sidebar width, agents section ratio, pane dividers | `app/WorkspaceDock.tsx:57`, `:83`, `features/shell/RightPanel.tsx:37`, `features/workspaces/WorkspaceSidebar.tsx:142-143` (all via `features/workspaces/transientDrag.ts:67`); pane dividers `app/App.tsx:1426-1430` | A number | In-app, a resize rather than a data drag |
| 5 | Title bar, `data-tauri-drag-region` | `features/shell/TitleBar.tsx:172-199`, `features/workspaces/TabStrip.tsx:259` (when in the title bar); capability `core:window:allow-start-dragging` | Nothing | **OS-bound, but it moves the window and carries no data.** GPUI needs a window-move area or `start_window_move`; this is the S3 client-decoration work, not an S8 data drag |

Why 1 and 2 cannot reach the OS even today:

- The only `setData` call in the tree writes the private MIME type above
  (`internalPathDrag.ts:68`). There is no `text/plain` or `text/uri-list`, so another
  program receives nothing it can use.
- The payload is a host path (`hostProfileId`, `serverIdentity`, `path`), usually a path
  on a remote machine, not a local file. `readInternalPathDrop` rejects it unless the
  target is a terminal on the same host (`internalPathDrag.ts:196-200`). The only
  consumers are the terminal transfer surface and the dock drop zones, both in the
  same window.

`app/WorkspaceDock.tsx` is **not** a drag source, which corrects the plan's S8 row. It
holds the dock drop zones that accept source 1 and 2 (`:177-195`), and the divider
resize (source 4).

Nothing deliberately writes a file path, URI, text or file into a `DataTransfer` for
another application, and the explorer cannot drag a file out to Finder or the desktop.
The styles set no `user-drag` rule, so WebKit's own default drags of links, images and
selected text in the Markdown preview are not suppressed, but no behavior is built on
them and the port does not need to reproduce them.

### Drop targets (OS to app, inbound)

These are the only places data crosses the OS boundary, and all of them are inbound.
They matter for S3 (file drop in), not S8.

| Where | What |
|---|---|
| `features/terminal/nativeDragDropCoordinator.ts:46` | Tauri `onDragDropEvent`: the only source of real local file paths, routed to registered targets by position |
| `features/terminal/TerminalTransferSurface.tsx:386`, `:476-510` | Terminal pane: internal path drop, then `x-special/gnome-copied-files`, `text/uri-list`, `File` objects, then a single image |
| `app/WorkspaceDock.tsx:157`, `:177-195` | Dock edge zones: internal path drops (HTML5), plus a native-lane claim that only recognizes our own drags (`:160`); files from the desktop are left to the terminal pane |

### Consequences for the port

- Sources 1 and 2 become in-process drag state. `internalPathDrag.ts` keeps its host
  check and path validation. The MIME type, `DataTransfer`, the `gestureId` round trip
  and the macOS native-claim bridge exist only because the DOM drag and Tauri's native
  drop event are two lanes for one gesture. With one lane they can go.
- Sources 3 and 4 already avoid HTML5 drag (`pointerDrag.ts` says why), so they port as
  mouse capture.
- GPUI must receive a file drop from the OS for the terminal and dock (S3, "file drop in").
  That is the one capability to verify, and it is a drop, not a drag source.

## Expected to retire: confirmed or overturned

The plan lists seven items. Four are confirmed (one with a carry-over note) and three
are overturned.

| Item | Verdict | Evidence |
|---|---|---|
| `TerminalRenderer` | **Overturned**: `replace`, not `retire` | `XtermRenderer` is replaced by `TerminalElement`, but the file also holds logic that must survive: `restoreDecision` (`:352`), `activatedTerminalUrl` (`:380`), pane grid reconciliation, resource recovery and `HistoryPageAnchor`. Its 42-case test file mixes xterm mechanics (retire) with these (port) |
| `TerminalWriteScheduler` | **Overturned**: `replace`, not `retire` | Frame pacing and the 200 ms occlusion fallback retire, but the bounded queue, overflow-to-reseed exactly once, fenced and partial records, and the rendered-checkpoint callback order are the contract the engine's admission step must keep before it acks host credit |
| `TerminalStateCache` | Confirmed `retire` | The serialize cache goes with `addon-serialize`. Carry-over: its `screenSeeded` and "reached top of history" flags (`TerminalStateCache.ts:22-45`) become plain pane state in the engine, and `PaneHistoryPager.ts:3` imports `CachedHistoryState` from it. `terminalCacheKey` is also used by `useActiveRoot.ts:5` and `useAppConnectionController.ts:29` |
| `atlasFontSmoothing` | Confirmed `retire` | Workaround for the xterm WebGL glyph atlas |
| `atlasStaleProbe` | Confirmed `retire` | Detects a stale shared WebGL atlas (`atlasModelInvalidation.test.ts` retires with it; `glyphWeight.test.ts` is `port`, because its cell-width cases outlive the atlas) |
| `panePaintGate` | **Overturned**: `replace`, not `retire` | It is not a rendering workaround. It delays the Explorer root probe until the pane's first paint so a slow link carries the terminal first (`panePaintGate.ts:1-24`). The need survives; only the DOM-paint trigger changes. Consumer: `features/files/useActiveRoot.ts:4` |
| `tests/performance/runtime/atlas-merge-repro` | Confirmed `retire` | `harness.js`, `index.html` and `run.mjs` reproduce the WebGL atlas merge. Outside `apps/desktop/src`, so it has no row here |

Also marked `retire`, beyond the plan's list: `TerminalBytes`,
`testing/RustPaneHost`, `fontGate`, `editorPreload`, `editorLayout`,
`usePerHostMemo`, `useCommittedRef`. `monaco` and `markdownPreview` are not retired
outright: `monaco.ts` carries the editor theme (`replace`), and `markdownPreview.ts`
carries the selection hold (`?`, below). `main.tsx` is `replace` because its startup
ordering stays.

## Open questions (`?` rows)

| File | Question |
|---|---|
| `features/files/LiveMarkdownEditor.tsx` | Depends on S4. Does the editor wrapper exist at all if Live Markdown lands on the Kit editor, or does it become the preview-beside-source fallback (decision 3) |
| `features/files/liveMarkdown.ts` | Depends on S4. CodeMirror decoration extension; the pure helpers port, the hiding mechanism may not. Its tests (`liveMarkdown.test.ts`, `LiveMarkdownEditor.test.tsx`) follow the answer |
| `features/files/liveTable.ts` | Depends on S4. Inline editable table; the cell-replace logic ports, the widget does not. `liveTable.test.ts` follows |
| `features/shell/MarkdownPreview.test.tsx` | Preview keeps the user's selection while content updates, using DOM selection events. Does the Kit Markdown view need the same behavior |
| `features/shell/appStateContract.test.ts` | Shared fixture moved to `crates/desktop-core/src/persistedAppState.contract.json` in Phase 1; which half of the test survives |
| `features/terminal/rendererResizeOrdering.test.ts` | Does the engine serialize a queued history rewrite and a resize by construction, or does the ordering need its own test. Answered in T3.3: by construction, one engine command loop |
| `features/terminal/rendererDrainBound.test.ts` | Does the engine keep a bounded hide drain with an honest checkpoint watermark? If hiding no longer serializes anything, three xterm-timed cases drop, but the frozen-pane contract still needs a test |
| `features/files/markdownPreview.ts` | `useSanitizedMarkdown` holds a refresh while the user is selecting in the preview, and `CLICK_SLOP_PX` separates a link click from a selection drag. Does the Kit Markdown view need either (same question as `MarkdownPreview.test.tsx`) |

## Not in the ledger

Tracked files under `apps/desktop/src` that are not `.ts`, `.tsx` or `.css` have no row
and need a decision in the phase that touches them: `crates/desktop-core/src/persistedAppState.contract.json`
(see above) and `assets/fonts/` (six `.woff2` files and two license texts; the plan's
font row covers them).

## Files

### `(src root)`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `main.tsx` | 73 | replace | 2 | GPUI app main: apply the remembered theme before first paint, report terminal colors to hosts at start and on appearance change, record the startup perf milestones; React, StrictMode and the font gate drop | open |
| `monaco-languages.d.ts` | 7 | retire | 5 | Monaco language module typings | open |
| `styles.css` | 2062 | replace | 2 | Rust theme and per-view styles (2,062 lines); its reduced-motion, forced-colors, narrow-width and 150 ms delayed-loading rules are behavior to carry | open |
| `tokens.css` | 117 | replace | 2 | generated Rust palette from client-core generator | open |
| `vite-env.d.ts` | 4 | retire | 9 | Vite client typings; the `__MUXFLOW_PERF_BUILD__` switch (set in `vite.config.ts`, not a row) needs a Rust equivalent in the perf work | open |

### `app`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `app/App.integration.test.tsx` | 241 | port | 2 | app-level connect and restore journeys on GPUI test context | open |
| `app/App.multiHost.test.tsx` | 593 | port | 2 | multi-host shell behavior | open |
| `app/App.tsx` | 2129 | port | 2 | app root entity; split into per-domain entities | open |
| `app/AppDialogLayer.tsx` | 122 | port | 2 | dialog layer over Kit dialogs | open |
| `app/AppNoticeLayer.tsx` | 52 | port | 2 | Kit notifications through `notices.rs`. One status notice at a time, a problem stays. The transfer toast is not here yet; it comes with transfers | open |
| `app/AppRightPanel.tsx` | 89 | port | 2 | right panel host (Explorer, Git, Ports rails) | open |
| `app/TerminalWorkspaceSurface.test.tsx` | 185 | port | 3 | separator rule and naming in terminal/layout.rs and surface.rs; divider, grab and join rules in terminal/integration/frames.rs (T3.6); key and pointer activity reports the pane through terminal/integration/agents.rs `note_activity`; the acknowledgement it drives is tested in agents/tests.rs (T4.2) | done (T4.2) |
| `app/TerminalWorkspaceSurface.tsx` | 193 | port | 3 | terminal/surface.rs: pane frames, resting dividers, empty state, region name, and (T3.6) divider drag and arrow-key resize, the attention ring (fed by the agents runtime in Phase 4), hover focus, selecting a pane in tmux on focus, and the link-modifier pane grab, which joins a pane beside another; the grab yields to a link once T3.5's link test is wired into `integration::link_at` | done (T3.6) |
| `app/WorkspaceDock.test.tsx` | 131 | port | 2 | tests of WorkspaceDock.tsx | open |
| `app/WorkspaceDock.tsx` | 232 | port | 2 | dock layout on Kit dock; also a drop target, not a drag source (see S8) | open |
| `app/appRecovery.test.ts` | 43 | port | 2 | tests in connection_ui/app_recovery.rs | done (T2.4) |
| `app/appRecovery.ts` | 51 | port | 2 | connection_ui/app_recovery.rs | done (T2.4) |
| `app/bridgeFailureText.test.ts` | 13 | port | 2 | tests in connection_ui/controller.rs | done (T2.4) |
| `app/bridgeFailureText.ts` | 12 | port | 2 | connection_ui/controller.rs user_facing_bridge_failure | done (T2.4) |
| `app/editorChunkAbsence.test.tsx` | 225 | port | 5 | the chunk-not-fetched probe drops; the visible outcomes port: binary file, file over 10 MiB, disconnected tab, binary diff and oversized diff show their message instead of an editor, and a Markdown preview-only tab shows no editor | open |
| `app/editorChunkBoundary.test.tsx` | 386 | port | 5 | chunk evaluation order and the 500 ms preview publish delay drop; port: dirty draft and its state survive a reconnect without an eager reread, a pending autosave survives the editor unmounting (preview switch), Live Markdown edits share one buffer with source, a live tab over the size limit falls back to source with live disabled | open |
| `app/helperSetupFeedback.test.tsx` | 82 | port | 2 | helper dialog tests in connection_ui/controller/tests.rs; hook-setup activity is Phase 4 with useAgentHostSetup | done (T2.4) |
| `app/linkQuality.test.ts` | 112 | port | 2 | tests in connection_ui/link_quality.rs | done (T2.4) |
| `app/linkQuality.ts` | 161 | port | 2 | connection_ui/link_quality.rs | done (T2.4) |
| `app/missingHelperRecovery.test.tsx` | 420 | port | 2 | connection_ui/controller/tests.rs | done (T2.4) |
| `app/multiHostBridges.test.tsx` | 412 | port | 2 | connection_ui/controller/tests.rs; prewarm, hub, reseed, clipboard, file, git and agent-stamping cases move with Phase 3/4 owners | done (T2.4) |
| `app/paneHoverFocus.test.tsx` | 160 | port | 3 | the delay and the after-drop quiet are in terminal/integration/frames.rs; the timer, field and window checks are view behavior in surface.rs, checked in T3.6 QA | done (T3.6) |
| `app/paneHoverFocus.ts` | 91 | port | 3 | hover focus in terminal/surface.rs: one decision per entry, rechecked when the timer fires, never taken from anything but another pane | done (T3.6) |
| `app/paneRouting.test.ts` | 44 | port | 3 | tests in terminal/integration/routing.rs | done (T3.6) |
| `app/paneRouting.ts` | 60 | port | 3 | terminal/integration/routing.rs; its callers are the agents and notifications in Phase 4 | done (T3.6) |
| `app/postResyncReseed.test.tsx` | 93 | port | 3 | reseed after resync | open |
| `app/reconnectNoticeDedupe.test.tsx` | 129 | port | 2 | connection_ui/controller/tests.rs | done (T2.4) |
| `app/shellNavigationCoordinator.ts` | 165 | port | 4 | shell navigation coordinator | open |
| `app/terminalFileOpenRoute.test.ts` | 36 | port | 3 | tests of terminalFileOpenRoute.ts | open |
| `app/terminalFileOpenRoute.ts` | 27 | port | 3 | terminal file open route check | open |
| `app/types.ts` | 68 | replace | 2 | Session, Window, Pane, ConnectionSpec, HostProfile come from protocol and desktop-core types | open |
| `app/useAppAgentController.ts` | 162 | port | 4 | agents/mod.rs: scopes, focus and setup gating from the connection controller, rows across hosts, the jump, row opening and the palette's row commands; the workflow half is agents/setup.rs. Open: launch and resume need the active root (`agents::view::ActiveRoot`, set by the Explorer in Phase 5); selecting a terminal tab should acknowledge its pane through `terminal::integration::agents::note_activity` (tab strip owner); bringing an agent's pane here and moving it go with pane moves | open |
| `app/useAppConnectionController.ts` | 944 | port | 2 | connection_ui/controller.rs; terminal hub and state cache, reseeds, echo-lag and input-latency probes, clipboard, file, git, agent and automation frame routing, retireConnection, appFocused and prewarm_terminal_bulk stay with Phase 3 and 4 owners; active-window resolution and the optimistic window with T4.1; after-install agent setup with useAgentHostSetup | done (T2.4) |
| `app/useAppFileActions.test.tsx` | 196 | port | 5 | tests of useAppFileActions.ts | open |
| `app/useAppFileActions.ts` | 145 | port | 5 | file actions entity (mutation, save panel, transfers) | open |
| `app/useAppHostSettingsActions.ts` | 179 | port | 4 | host connection, profile and helper actions in connection_ui/controller.rs (T2.4); the rest of the host settings actions land with Phase 4 | open |
| `app/useAppRecoveryController.test.tsx` | 112 | port | 2 | connection_ui/controller/tests.rs | done (T2.4) |
| `app/useAppRecoveryController.ts` | 104 | port | 2 | connection_ui/controller.rs; T4.1 rewires its app-state access to the single in-memory owner and adds the flush on close | done (T2.4) |
| `app/useAppShellChrome.test.tsx` | 91 | port | 2 | tests of useAppShellChrome.ts | open |
| `app/useAppShellChrome.ts` | 59 | port | 2 | rail layout and status-to-notice lifecycle | open |
| `app/useBulkTabClose.test.tsx` | 117 | port | 4 | tests of useBulkTabClose.ts | open |
| `app/useBulkTabClose.ts` | 145 | port | 4 | bulk tab close | open |
| `app/useClientResize.test.tsx` | 421 | port | 3 | tests in terminal/client_size.rs | done (T3.3) |
| `app/useClientResize.ts` | 320 | port | 3 | terminal/client_size.rs (ClientResize) driven by terminal/surface.rs | done (T3.3) |
| `app/useFocusHistoryNavigation.test.tsx` | 190 | port | 4 | tests of useFocusHistoryNavigation.ts | open |
| `app/useFocusHistoryNavigation.ts` | 135 | port | 4 | focus history navigation | open |
| `app/useMissingHelperRecovery.ts` | 176 | port | 2 | connection_ui/helper_recovery.rs | done (T2.4) |
| `app/usePaneMoves.test.tsx` | 160 | port | 3 | tests of usePaneMoves.ts; the pane-onto-pane join is tested in terminal/integration/frames.rs (T3.6) | open |
| `app/usePaneMoves.ts` | 201 | port | 3 | pane moves via drop_rules; the pane grab joining a pane beside another is in terminal/surface.rs (T3.6), tab and workspace drops come with drop_rules in Phase 4 | open |
| `app/usePerHostMemo.test.tsx` | 61 | retire | 2 | tests usePerHostMemo.ts, which retires | open |
| `app/usePerHostMemo.ts` | 56 | retire | 2 | React hook that memoizes a list per host; entities hold per-host state directly | open |
| `app/useShellNavigation.test.tsx` | 1378 | port | 4 | tests of useShellNavigation.ts | open |
| `app/useShellNavigation.ts` | 735 | port | 4 | shell navigation | open |
| `app/useTerminalFileOpen.ts` | 108 | port | 3 | terminal file open; needs file tabs (Phase 4 and 5): until then a file link click says it is not available (terminal/reading/mod.rs) | open |
| `app/useTmuxActionPerformer.test.tsx` | 187 | port | 4 | tests of useTmuxActionPerformer.ts | open |
| `app/useTmuxActionPerformer.ts` | 146 | port | 4 | tmux action performer | open |
| `app/useWorkspaceCreate.test.tsx` | 142 | port | 4 | tests of useWorkspaceCreate.ts | open |
| `app/useWorkspaceCreate.ts` | 69 | port | 4 | workspace create with host defaults | open |
| `app/useWorkspaceDomainController.test.ts` | 27 | port | 4 | tests of useWorkspaceDomainController.ts | open |
| `app/useWorkspaceDomainController.ts` | 99 | port | 4 | workspace domain controller | open |
| `app/windowSelection.test.ts` | 79 | port | 4 | tests of windowSelection.ts | open |
| `app/windowSelection.ts` | 97 | port | 4 | window selection and reorder | open |

### `commands`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `commands/CommandPalette.tsx` | 126 | port | 2 | `commands/palette.rs`: own list, not Kit `Command`, whose virtualized rows leave offscreen commands out of the accessibility tree | open |
| `commands/ConfirmationDialog.tsx` | 49 | replace | 2 | Kit alert dialog, `commands/dialogs.rs`. Cancel holds focus, so Enter cancels. Non-tmux `busy` state is not needed yet | open |
| `commands/NewWorkspaceDialog.test.tsx` | 118 | port | 4 | tests of NewWorkspaceDialog.tsx | open |
| `commands/NewWorkspaceDialog.tsx` | 113 | port | 4 | new workspace dialog | open |
| `commands/ShortcutEditorDialog.test.tsx` | 43 | port | 2 | `tests/commands.rs` | open |
| `commands/ShortcutEditorDialog.tsx` | 53 | port | 2 | `commands/shortcut_editor.rs` | open |
| `commands/TextInputDialog.tsx` | 34 | replace | 2 | Kit dialog, `commands/dialogs.rs`; Enter does not submit while composing | open |
| `commands/destructiveConfirmation.test.ts` | 84 | port | 2 | title and detail in `tests/commands.rs`; the action checks land in Phase 4 | open |
| `commands/destructiveConfirmation.ts` | 45 | port | 2 | title and detail in `commands/dialogs.rs`; the action checks need `TmuxAction`, so they land with `features/tmux/actions.ts` in Phase 4 | open |
| `commands/fuzzy.test.ts` | 43 | port | 2 | `commands/fuzzy.rs` | open |
| `commands/fuzzy.ts` | 61 | port | 2 | `commands/fuzzy.rs` | open |
| `commands/registry.test.ts` | 524 | port | 2 | `commands/{registry,keymap}.rs`, `tests/commands.rs` | open |
| `commands/registry.ts` | 583 | port | 2 | `commands/registry.rs`, `commands/keymap.rs`; keys matched against the portable form through `intercept_keystrokes`, no GPUI bindings. Editor fallback and the docked-document chords wait for Phase 5; the terminal's IME check is `terminal::input::is_composing` (T3.4). GPUI reports no physical key, so digit selectors on layouts with shifted digits (AZERTY) do not resolve | open |
| `commands/rowCommands.test.ts` | 54 | port | 2 | `tests/commands.rs` | open |
| `commands/rowCommands.ts` | 99 | port | 2 | `commands/mod.rs` `publish_row_commands` | open |
| `commands/shortcutGlyphs.test.ts` | 47 | port | 2 | `commands/glyphs.rs` | open |
| `commands/shortcutGlyphs.ts` | 106 | port | 2 | `commands/glyphs.rs` | open |
| `commands/useCommittedRef.ts` | 29 | retire | 2 | React stale-closure workaround; entities read live state | open |
| `commands/useModalDialog.test.tsx` | 67 | port | 2 | focus trap and inert-background cases drop (Kit dialog); Escape and Enter during IME composition ported to `tests/commands.rs` | open |
| `commands/useModalDialog.ts` | 92 | replace | 2 | Kit dialog focus trap and focus return; `dialogs::escape_ends_composition` covers Escape during IME composition | open |

### `diagnostics`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `diagnostics/copyChordJournal.test.ts` | 44 | port | 3 | tests of copyChordJournal.ts | open |
| `diagnostics/copyChordJournal.ts` | 37 | port | 3 | Ctrl+C copy-then-interrupt journal | open |
| `diagnostics/incidents.test.ts` | 47 | port | 2 | behavior assertions move to the replacement of incidents.ts | open |
| `diagnostics/incidents.ts` | 39 | replace | 2 | direct call into desktop-core incidents (renderer half of incidents.rs) | open |

### `features/agents`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/agents/AgentHookWorkflow.tsx` | 165 | port | 4 | agents/setup.rs (review, confirm, cancel; bound to the host the review was loaded for) with the dialog in agents/view.rs | done (T4.2) |
| `features/agents/AgentHostSetupDialog.tsx` | 77 | port | 4 | agents/view.rs `open_setup_dialog`, with the Tauri copy | done (T4.2) |
| `features/agents/AgentIdentity.test.tsx` | 75 | port | 4 | label cases in agents/labels.rs; the marks and the docked state are view behavior, checked in T4.2 QA | done (T4.2) |
| `features/agents/AgentIdentity.tsx` | 85 | port | 4 | agents/view.rs `agent_mark` (adapter SVGs drawn as masks, state docked or beside with glyphs); labels in agents/labels.rs | done (T4.2) |
| `features/agents/HookReviewDialog.tsx` | 49 | port | 4 | agents/view.rs `open_review_dialog`, with the Tauri copy | done (T4.2) |
| `features/agents/adapterDefinitions.ts` | 6 | port | 4 | the adapter ids are matched where they are used (agents/view.rs marks, agents/labels.rs) | done (T4.2) |
| `features/agents/agentLabels.test.ts` | 87 | port | 4 | tests in agents/labels.rs | done (T4.2) |
| `features/agents/agentLabels.ts` | 1 | port | 4 | agents/labels.rs; the mobile app keeps its own client-core copy | done (T4.2) |
| `features/agents/agentStatusSurfaces.test.tsx` | 279 | port | 4 | the notice and wiring cases in agents/wiring.rs, the row and selection cases in agents/list.rs; dot shapes, slots and stylesheet cases are view behavior, checked in T4.2 QA | done (T4.2) |
| `features/agents/agentsList.test.ts` | 402 | port | 4 | tests in agents/list.rs | done (T4.2) |
| `features/agents/agentsList.ts` | 331 | port | 4 | agents/list.rs | done (T4.2) |
| `features/agents/api.test.ts` | 118 | port | 4 | tests in agents/wire.rs | done (T4.2) |
| `features/agents/api.ts` | 407 | replace | 4 | agents/wire.rs maps commands and answers; requests go through `TerminalBackend::agent_request` to desktop-core | done (T4.2) |
| `features/agents/catchUp.test.tsx` | 100 | port | 4 | reconnect catch-up (an advanced watermark notifies once) in agents/tests.rs | done (T4.2) |
| `features/agents/generation.test.ts` | 20 | port | 4 | full-range u64 generations in agents/wire.rs tests | done (T4.2) |
| `features/agents/generation.ts` | 29 | port | 4 | generations are `u64` (agents/model.rs `Generation`), parsed in agents/wire.rs | done (T4.2) |
| `features/agents/hookWiring.test.ts` | 92 | port | 4 | tests in agents/wiring.rs | done (T4.2) |
| `features/agents/hookWiring.ts` | 88 | port | 4 | agents/wiring.rs | done (T4.2) |
| `features/agents/notifications.test.ts` | 117 | port | 4 | tests in agents/notify.rs | done (T4.2) |
| `features/agents/notifications.ts` | 169 | replace | 4 | agents/notify.rs decides; desktop-core shows the notification (agents/runtime.rs `NativeEffects`) | done (T4.2) |
| `features/agents/runtimeMemory.test.ts` | 23 | port | 4 | tests in agents/model.rs | done (T4.2) |
| `features/agents/runtimeMemory.ts` | 69 | port | 4 | agents/model.rs `RuntimeMemory` | done (T4.2) |
| `features/agents/selectors.test.ts` | 54 | port | 4 | tests in agents/model.rs | done (T4.2) |
| `features/agents/selectors.ts` | 97 | port | 4 | agents/model.rs selectors and rollups | done (T4.2) |
| `features/agents/sound.test.ts` | 23 | port | 4 | tests in agents/sound.rs; the localStorage import retires with the WebView | done (T4.2) |
| `features/agents/sound.ts` | 70 | replace | 4 | agents/sound.rs: the same two cues synthesized and played with rodio, preferences read from app state when a cue plays | done (T4.2) |
| `features/agents/state.test.ts` | 233 | port | 4 | tests in agents/model.rs | done (T4.2) |
| `features/agents/state.ts` | 153 | port | 4 | agents/model.rs `AgentStore` | done (T4.2) |
| `features/agents/testFixtures.ts` | 20 | port | 4 | agents/model.rs `fixtures` | done (T4.2) |
| `features/agents/types.ts` | 293 | port | 4 | agents/model.rs | done (T4.2) |
| `features/agents/useAgentHostSetup.test.tsx` | 624 | port | 4 | prompt, decline, install, frozen state, helper auto-setup and reassert cases in agents/tests.rs; host binding through the `consented_scope` refusal there (a switch between review and apply takes the same check) | done (T4.2) |
| `features/agents/useAgentHostSetup.tsx` | 448 | port | 4 | agents/setup.rs | done (T4.2) |
| `features/agents/useAgentNotificationActivation.test.tsx` | 115 | port | 4 | route, stale-error and resolution cases in agents/activation.rs; the focus and exact acknowledgement flow is checked in T4.2 QA | done (T4.2) |
| `features/agents/useAgentNotificationActivation.ts` | 146 | port | 4 | agents/activation.rs: clicks are queued off the notification thread and polled | done (T4.2) |
| `features/agents/useAgentRuntime.test.tsx` | 505 | port | 4 | tests in agents/tests.rs against a scripted host | done (T4.2) |
| `features/agents/useAgentRuntime.ts` | 378 | port | 4 | agents/runtime.rs. Open: the topology authority (the tmux window set an agent snapshot was paired with, which `useBulkTabClose.ts` uses to protect agent tabs) is not produced; it lands with the bulk tab close port | open |
| `features/agents/useRecentIdleClock.test.tsx` | 85 | port | 4 | expiration cases in agents/list.rs | done (T4.2) |
| `features/agents/useRecentIdleClock.ts` | 33 | port | 4 | agents/runtime.rs `arm_recent_clock`: one timer at the nearest expiration | done (T4.2) |

### `features/automations`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/automations/AutomationEditor.tsx` | 265 | port | 4 | automations/panel.rs (draft, save, delete, stale handling) and automations/view.rs (dialog), with the Tauri copy | done (T4.2) |
| `features/automations/AutomationsPanel.test.tsx` | 317 | port | 4 | tests in automations/tests.rs against a scripted helper | done (T4.2) |
| `features/automations/AutomationsPanel.tsx` | 342 | port | 4 | automations/panel.rs and automations/view.rs, the right panel's "automations" segment | done (T4.2) |
| `features/automations/api.ts` | 46 | replace | 4 | requests go through `TerminalBackend::automation_request` to desktop-core; refusal helpers in automations/model.rs | done (T4.2) |
| `features/automations/cronText.test.ts` | 35 | port | 4 | tests in automations/model.rs | done (T4.2) |
| `features/automations/cronText.ts` | 92 | port | 4 | automations/model.rs | done (T4.2) |
| `features/automations/types.ts` | 60 | port | 4 | automations/model.rs | done (T4.2) |

### `features/dragDrop`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/dragDrop/DragGhost.tsx` | 11 | port | 4 | drag ghost view | open |
| `features/dragDrop/dropRules.test.ts` | 163 | port | 4 | tests of dropRules.ts | open |
| `features/dragDrop/dropRules.ts` | 158 | port | 4 | drop planning rules | open |
| `features/dragDrop/pointerDrag.test.ts` | 190 | port | 4 | tests of pointerDrag.ts | open |
| `features/dragDrop/pointerDrag.ts` | 277 | port | 4 | pointer drag state; mouse capture instead of DOM pointer capture | open |

### `features/files`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/files/DownloadActions.tsx` | 28 | port | 5 | download completion actions | open |
| `features/files/DownloadTransfers.test.tsx` | 109 | port | 5 | tests of DownloadTransfers.tsx | open |
| `features/files/DownloadTransfers.tsx` | 90 | port | 5 | downloads list view | open |
| `features/files/ExplorerMutationDialog.test.tsx` | 32 | port | 5 | tests of ExplorerMutationDialog.tsx | open |
| `features/files/ExplorerMutationDialog.tsx` | 148 | port | 5 | explorer mutation dialogs | open |
| `features/files/ExplorerRow.tsx` | 131 | port | 5 | explorer row; has an in-app drag source (see S8) | open |
| `features/files/ExplorerTree.test.tsx` | 760 | port | 5 | tests of ExplorerTree.tsx | open |
| `features/files/ExplorerTree.tsx` | 624 | port | 5 | explorer tree view; has an in-app drag source (see S8) | open |
| `features/files/FileEditor.tsx` | 57 | replace | 5 | Kit code editor wrapper replaces the Monaco wrapper | open |
| `features/files/LiveMarkdownEditor.test.tsx` | 117 | port | 7 | tests of LiveMarkdownEditor.tsx | open |
| `features/files/LiveMarkdownEditor.tsx` | 154 | port | 7 | ? depends on S4: does Live Markdown fit the Kit editor or gpui-base text | open |
| `features/files/api.test.ts` | 506 | port | 5 | behavior assertions move to the replacement of api.ts | open |
| `features/files/api.ts` | 647 | replace | 5 | Tauri file client becomes direct desktop-core calls; its request validation moves with it | open |
| `features/files/autosave.test.ts` | 104 | port | 5 | tests of autosave.ts | open |
| `features/files/autosave.ts` | 108 | port | 5 | autosave controller | open |
| `features/files/directoryCache.test.ts` | 57 | port | 5 | tests of directoryCache.ts | open |
| `features/files/directoryCache.ts` | 103 | port | 5 | explorer directory cache | open |
| `features/files/directoryRequests.test.ts` | 82 | port | 5 | tests of directoryRequests.ts | open |
| `features/files/directoryRequests.ts` | 119 | port | 5 | directory request coalescing | open |
| `features/files/directoryState.test.ts` | 223 | port | 5 | tests of directoryState.ts | open |
| `features/files/directoryState.ts` | 205 | port | 5 | directory state model | open |
| `features/files/downloadFlow.test.ts` | 78 | port | 5 | tests of downloadFlow.ts | open |
| `features/files/downloadFlow.ts` | 73 | port | 5 | download flow | open |
| `features/files/downloadStatus.test.ts` | 67 | port | 5 | tests of downloadStatus.ts | open |
| `features/files/downloadStatus.ts` | 37 | port | 5 | download status | open |
| `features/files/editorFlushRegistry.test.ts` | 27 | port | 5 | tests of editorFlushRegistry.ts | open |
| `features/files/editorFlushRegistry.ts` | 26 | port | 5 | editor flush registry | open |
| `features/files/editorLanguage.test.ts` | 31 | port | 5 | tests of editorLanguage.ts | open |
| `features/files/editorLanguage.ts` | 34 | port | 5 | extension to Tree-sitter grammar table | open |
| `features/files/editorLayout.test.tsx` | 118 | retire | 5 | tests editorLayout.ts, which retires | open |
| `features/files/editorLayout.ts` | 99 | retire | 5 | forces Monaco to re-measure its container; no equivalent layout step in GPUI | open |
| `features/files/editorText.ts` | 18 | replace | 5 | Monaco font options; becomes editor font settings from the theme | open |
| `features/files/explorerWindow.test.ts` | 158 | port | 5 | tests of explorerWindow.ts | open |
| `features/files/explorerWindow.ts` | 120 | port | 5 | explorer virtual window (4,096 entries gate) | open |
| `features/files/fileIcons.test.ts` | 58 | port | 5 | tests of fileIcons.ts | open |
| `features/files/fileIcons.ts` | 91 | port | 5 | file icon mapping | open |
| `features/files/ignoredPaths.test.ts` | 76 | port | 5 | tests of ignoredPaths.ts | open |
| `features/files/ignoredPaths.ts` | 58 | port | 5 | gitignore toggle model | open |
| `features/files/listingModel.test.ts` | 165 | port | 5 | tests of listingModel.ts | open |
| `features/files/listingModel.ts` | 173 | port | 5 | listing model | open |
| `features/files/liveMarkdown.test.ts` | 776 | port | 7 | tests of liveMarkdown.ts | open |
| `features/files/liveMarkdown.ts` | 1176 | port | 7 | ? depends on S4: CodeMirror decoration extension; pure helpers (block markup, list indent, link targets) port, hiding mechanism does not | open |
| `features/files/liveTable.test.ts` | 159 | port | 7 | tests of liveTable.ts | open |
| `features/files/liveTable.ts` | 535 | port | 7 | ? depends on S4: inline editable table; cell-replace logic ports, widget mechanism does not | open |
| `features/files/markdown.test.ts` | 91 | port | 5 | keep markdownFilePath cases; drop the sanitizer cases with DOMPurify | open |
| `features/files/markdown.ts` | 32 | port | 5 | markdownFilePath link resolution ports; renderSafeMarkdown and renderSafeSvg retire with DOMPurify | open |
| `features/files/markdownPreview.test.ts` | 66 | retire | 5 | tests the sanitize debounce and idle scheduling, which retire with DOMPurify | open |
| `features/files/markdownPreview.ts` | 164 | port | 5 | ? the debounced DOMPurify pass drops (Kit renders natively); `useSanitizedMarkdown`'s selection hold and `CLICK_SLOP_PX` (used by AppTabSurface) port only if the Kit Markdown view needs them, see MarkdownPreview.test.tsx | open |
| `features/files/monaco.ts` | 152 | replace | 5 | editor theme from the generated palette (syntax colors from the terminal ANSI palette, comments dim, selection, cursor and diff washes, light and dark); Monaco workers, loader and font remeasure drop (open decision 2) | open |
| `features/files/monarch.test.ts` | 93 | port | 5 | tests of monarch.ts | open |
| `features/files/monarch.ts` | 109 | port | 5 | Python and TypeScript highlight additions become Tree-sitter highlight queries | open |
| `features/files/saveStatus.test.tsx` | 229 | port | 5 | tests of saveStatus.ts | open |
| `features/files/saveStatus.ts` | 51 | port | 5 | calm save status projection | open |
| `features/files/searchCount.test.ts` | 79 | port | 5 | tests of searchCount.ts | open |
| `features/files/searchCount.ts` | 85 | port | 5 | search match count with limit | open |
| `features/files/types.ts` | 280 | port | 5 | file types, limits, wire shapes | open |
| `features/files/useActiveRoot.ts` | 280 | port | 5 | active root resolution; waits on pane paint gate | open |
| `features/files/useConnectionTransfers.ts` | 85 | port | 5 | per-connection download transfers | open |
| `features/files/useOpenFileTab.ts` | 504 | port | 5 | open file tab flow | open |
| `features/files/useWorkspaceFiles.test.tsx` | 1402 | port | 5 | tests of useWorkspaceFiles.ts | open |
| `features/files/useWorkspaceFiles.ts` | 908 | port | 5 | workspace files controller | open |
| `features/files/watchLeases.test.ts` | 208 | port | 5 | tests of watchLeases.ts | open |
| `features/files/watchLeases.ts` | 181 | port | 5 | watch leases | open |
| `features/files/webkitWidgetBuffers.test.ts` | 43 | retire | 5 | CodeMirror widget buffer behavior in WebKit | open |
| `features/files/wire.ts` | 135 | port | 5 | file wire decode and transfer state validation | open |

### `features/git`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/git/GitCommitForm.tsx` | 166 | port | 5 | commit form view | open |
| `features/git/GitDiffEditor.test.tsx` | 39 | retire | 5 | asserts Monaco diff option values through a mocked wrapper; wrap and alignment are covered by the Phase 5 diff exit gate | open |
| `features/git/GitDiffEditor.tsx` | 60 | replace | 5 | Kit Diff (read-only, virtualized) with a patch from similar | open |
| `features/git/GitDiffSurface.test.tsx` | 350 | port | 5 | tests of GitDiffSurface.tsx | open |
| `features/git/GitDiffSurface.tsx` | 237 | port | 5 | diff surface with wrapped aligned columns | open |
| `features/git/GitSidebar.test.tsx` | 588 | port | 5 | tests of GitSidebar.tsx | open |
| `features/git/GitSidebar.tsx` | 450 | port | 5 | git sidebar; has an in-app drag source (see S8) | open |
| `features/git/api.test.ts` | 169 | port | 5 | behavior assertions move to the replacement of api.ts | open |
| `features/git/api.ts` | 559 | replace | 5 | Tauri git client becomes direct desktop-core calls | open |
| `features/git/repositoryStore.test.ts` | 306 | port | 5 | tests of repositoryStore.ts | open |
| `features/git/repositoryStore.ts` | 470 | port | 5 | repository store | open |
| `features/git/types.ts` | 144 | port | 5 | git types | open |
| `features/git/useSharedGitDiff.ts` | 268 | port | 5 | shared git diff controller | open |
| `features/git/useWorkspaceGit.test.tsx` | 163 | port | 5 | tests of useWorkspaceGit.ts | open |
| `features/git/useWorkspaceGit.ts` | 78 | port | 5 | workspace git controller | open |

### `features/ports`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/ports/PortsPanel.test.tsx` | 118 | port | 4 | tests of PortsPanel.tsx | open |
| `features/ports/PortsPanel.tsx` | 162 | port | 4 | ports panel view | open |
| `features/ports/api.ts` | 38 | replace | 4 | invoke wrapper becomes direct desktop-core calls; parsePort ports | open |
| `features/ports/usePortForwards.ts` | 37 | port | 4 | port forwards entity | open |

### `features/shell`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/shell/AppTabSurface.test.tsx` | 644 | port | 4 | tests of AppTabSurface.tsx | open |
| `features/shell/AppTabSurface.tsx` | 366 | port | 4 | app tab surface (document, diff, preview routing) | open |
| `features/shell/DisconnectedStrip.tsx` | 75 | port | 2 | connection_ui/surface.rs | done (T2.4) |
| `features/shell/MarkdownPreview.test.tsx` | 346 | port | 5 | ? preview keeps selection while content updates; DOM selection mechanics may not apply, keep the behavior | open |
| `features/shell/RightPanel.tsx` | 120 | port | 2 | right panel container with resizable width | open |
| `features/shell/SettingsDialog.tsx` | 611 | port | 4 | settings dialog | open |
| `features/shell/TitleBar.test.tsx` | 57 | port | 2 | tests of TitleBar.tsx | open |
| `features/shell/TitleBar.tsx` | 206 | port | 2 | title bar on GPUI client decorations | open |
| `features/shell/appStateContract.test.ts` | 334 | port | 4 | ? TS half of the shared app-state fixture retires (Rust half stays in app_state.rs); normalize and clamp cases port | open |
| `features/shell/appStatePersistence.test.ts` | 69 | port | 2 | tests of appStatePersistence.ts | open |
| `features/shell/appStatePersistence.ts` | 65 | port | 2 | serialized app_state saver with close barrier | open |
| `features/shell/focusHistory.test.ts` | 98 | port | 4 | tests of focusHistory.ts | open |
| `features/shell/focusHistory.ts` | 125 | port | 4 | focus history | open |
| `features/shell/helperUpgrade.test.ts` | 78 | port | 2 | tests in connection_ui/helper_upgrade.rs | done (T2.4) |
| `features/shell/helperUpgrade.ts` | 89 | port | 2 | connection_ui/helper_upgrade.rs | done (T2.4) |
| `features/shell/hostLatency.test.ts` | 39 | port | 2 | tests of hostLatency.ts | open |
| `features/shell/hostLatency.ts` | 72 | port | 2 | host latency readout | open |
| `features/shell/hostProfiles.test.ts` | 30 | port | 2 | tests in connection_ui/host_profiles.rs | done (T2.4) |
| `features/shell/hostProfiles.ts` | 31 | port | 2 | connection_ui/host_profiles.rs | done (T2.4) |
| `features/shell/hostScope.test.ts` | 43 | port | 2 | tests in connection_ui/host_scope.rs | done (T2.4) |
| `features/shell/hostScope.ts` | 47 | port | 2 | connection_ui/host_scope.rs | done (T2.4) |
| `features/shell/model.test.ts` | 806 | port | 4 | tests of model.ts | open |
| `features/shell/model.ts` | 1018 | port | 4 | shell model (tabs, workspaces, layout) | open |
| `features/shell/panelExtensions.ts` | 48 | port | 4 | panel extension host interface | open |
| `features/shell/responsiveShell.test.ts` | 86 | port | 2 | tests of responsiveShell.ts | open |
| `features/shell/responsiveShell.ts` | 67 | port | 2 | responsive rail layout | open |
| `features/shell/shellComponents.test.tsx` | 2301 | port | 4 | shell component behavior; React render mechanics cases dropped while porting | open |
| `features/shell/statusNotice.test.ts` | 94 | port | 2 | `notices.rs` unit tests | done (T2.4, T2.5) |
| `features/shell/statusNotice.ts` | 96 | port | 2 | `notices.rs`; one notice at a time, a problem stays until dismissed | done (T2.4, T2.5) |
| `features/shell/types.ts` | 458 | port | 4 | shell and persisted app-state types with normalization | open |
| `features/shell/useAppearance.ts` | 33 | port | 2 | applies saved theme once state is ready | open |
| `features/shell/useDesktopResumeRecovery.test.ts` | 81 | port | 2 | tests in connection_ui/resume_recovery.rs | done (T2.4) |
| `features/shell/useDesktopResumeRecovery.ts` | 151 | port | 2 | connection_ui/resume_recovery.rs; the browser online event is polled from getifaddrs on the 5 s check | done (T2.4) |
| `features/shell/usePersistedAppState.test.tsx` | 89 | port | 4 | tests of usePersistedAppState.ts | open |
| `features/shell/usePersistedAppState.ts` | 123 | port | 2 | persisted app state loader | open |
| `features/shell/useShellCommands.test.tsx` | 384 | port | 2 | dispatch in `tests/commands.rs`; each command body tested with its feature | open |
| `features/shell/useShellCommands.ts` | 443 | port | 2 | `commands/mod.rs` `run` and `on_command`: each command body lands with its feature (Phases 3 to 5) | open |
| `features/shell/useUpdateCheck.test.ts` | 95 | port | 2 | tests of useUpdateCheck.ts | open |
| `features/shell/useUpdateCheck.ts` | 97 | port | 2 | update check notice over kept update_check.rs | open |
| `features/shell/useWindowChrome.test.tsx` | 85 | port | 2 | `ui/title_bar.rs` unit tests of `classify`. Window-controls presence is read from the window's decorations | open |
| `features/shell/useWindowChrome.ts` | 81 | replace | 2 | `ui/title_bar.rs`: automatic selection, bare on a tiling desktop. The two environment overrides are removed | open |
| `features/shell/windowGeometry.test.ts` | 40 | port | 2 | `window_state.rs` unit tests, `tests/native.rs` round trip | open |
| `features/shell/windowGeometry.ts` | 85 | port | 2 | `window_state.rs`: saved through `update_app_state`, in the format Tauri wrote | open |

### `features/terminal`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/terminal/DeferredTerminalOutputQueue.test.ts` | 29 | port | 3 | tests in crates/terminal-engine/src/deferred.rs | done (T3.1) |
| `features/terminal/DeferredTerminalOutputQueue.ts` | 57 | port | 3 | crates/terminal-engine/src/deferred.rs | done (T3.1) |
| `features/terminal/PaneDegradedWatchdog.test.ts` | 102 | port | 3 | tests in crates/terminal-engine/src/watchdog.rs | done (T3.1) |
| `features/terminal/PaneDegradedWatchdog.ts` | 185 | port | 3 | crates/terminal-engine/src/watchdog.rs | done (T3.1) |
| `features/terminal/PaneHistoryPager.test.ts` | 358 | port | 3 | tests in crates/terminal-engine/src/pager.rs; the held-through-splice case retires because the splice is synchronous | done (T3.1) |
| `features/terminal/PaneHistoryPager.ts` | 500 | port | 3 | crates/terminal-engine/src/pager.rs; also refuses the clamped row tmux answers for a pane with no history | done (T3.1) |
| `features/terminal/PaneRecovery.ts` | 35 | port | 3 | crates/terminal-engine/src/reveal.rs | done (T3.1) |
| `features/terminal/PaneRevealState.test.ts` | 161 | port | 3 | tests in crates/terminal-engine/src/reveal.rs | done (T3.1) |
| `features/terminal/PaneRevealState.ts` | 154 | port | 3 | crates/terminal-engine/src/reveal.rs | done (T3.1) |
| `features/terminal/TerminalBytes.ts` | 18 | retire | 3 | branded owned-bytes type for a JS copy boundary; Rust ownership makes it a language fact | open |
| `features/terminal/TerminalEventHub.test.ts` | 812 | port | 3 | crates/terminal-engine/src/hub_tests.rs; consumer rejection becomes catch_unwind in engine.rs, which has no injection point for a test; observer failures have no equivalent | done (T3.1) |
| `features/terminal/TerminalEventHub.ts` | 844 | port | 3 | crates/terminal-engine/src/hub.rs | done (T3.1) |
| `features/terminal/TerminalGenerationWatermark.ts` | 40 | port | 3 | applied_generation in crates/terminal-engine/src/pane.rs; writes are synchronous, so no version guard is needed | done (T3.1) |
| `features/terminal/TerminalPane.test.tsx` | 1810 | port | 3 | tests of TerminalPane.tsx; the clipboard chord cases (copy then drop the selection, Mac Super+C) are in tests/terminal_reading.rs (T3.5) | open |
| `features/terminal/TerminalPane.tsx` | 1621 | port | 3 | event consumer and reveal driver in crates/terminal-engine/src/pane.rs and engine.rs (T3.1); pane entity in terminal/pane.rs and terminal/link.rs, element in terminal/element.rs (T3.3); reconcilePaneGrid in terminal/surface.rs (T3.3); copy, find, the new-output button and return to live in terminal/reading (T3.5); input and integration behavior go to T3.4, T3.6 | open |
| `features/terminal/TerminalPaneHostHandshake.test.tsx` | 810 | port | 3 | crates/terminal-engine/src/engine_tests.rs (T3.1); the grid-mismatch and double-mount cases go with the terminal view | open |
| `features/terminal/TerminalReadingState.test.ts` | 71 | port | 3 | tests in crates/terminal-engine/src/reading.rs | done (T3.1) |
| `features/terminal/TerminalReadingState.ts` | 57 | port | 3 | crates/terminal-engine/src/reading.rs | done (T3.1) |
| `features/terminal/TerminalRenderer.test.ts` | 915 | port | 3 | split: scheduler, xterm callbacks, paste interception retire; pane grid reconciliation ported to terminal/surface.rs (T3.3); restoreDecision is in terminal-engine (T3.1); seed recovery is in terminal-engine (T3.1) and no GPU resource recovery is needed; the search and `activatedTerminalUrl` cases are in terminal/reading/find.rs and links.rs (T3.5) | open |
| `features/terminal/TerminalRenderer.ts` | 1453 | replace | 3 | XtermRenderer becomes TerminalElement (T3.3); restoreDecision is in terminal-engine (T3.1); pane grid reconciliation in terminal/surface.rs (T3.3); selection, search, the link provider, the OSC 8 `linkHandler` and their activation, the scrollbar and top-of-history detection in terminal/reading (T3.5); no GPU resource recovery is needed in a GPUI element; a paste goes through the host's tmux paste path, which brackets it, instead of xterm's client bracketing (T3.8) | open |
| `features/terminal/TerminalStateCache.test.ts` | 140 | retire | 3 | tests TerminalStateCache.ts, which retires | done (T3.1) |
| `features/terminal/TerminalStateCache.ts` | 191 | retire | 3 | xterm serialize cache; hidden panes keep their live grids in crates/terminal-engine/src/engine.rs, bounded at 64 screens and 128 MiB, with the pager flags carried alongside | done (T3.1) |
| `features/terminal/TerminalTransferSurface.test.tsx` | 906 | port | 3 | batch tests in apps/desktop-gpui/tests/terminal_integration.rs; DOM paste events, the WebView image encoder and point hit-testing retire with the WebView | done (T3.6) |
| `features/terminal/TerminalTransferSurface.tsx` | 638 | port | 3 | terminal/integration/mod.rs (paste, drop, upload list) and upload.rs (the batch and its review dialog) | done (T3.6) |
| `features/terminal/TerminalWriteScheduler.test.ts` | 499 | port | 3 | write and acknowledgement tests in crates/terminal-engine/src/terminal.rs and engine_tests.rs; fences, partial records and frame pacing have no equivalent | done (T3.1) |
| `features/terminal/TerminalWriteScheduler.ts` | 613 | replace | 3 | synchronous parse in stream order in crates/terminal-engine/src/terminal.rs; a frame is acknowledged after it is applied, so the host's delivery window is the bound and nothing can overflow | done (T3.1) |
| `features/terminal/accessibilityPreference.ts` | 25 | retire | 3 | owner exception: the screen-reader terminal mirror and its setting are not built | open |
| `features/terminal/api.test.ts` | 748 | port | 3 | behavior assertions move to the replacement of api.ts | open |
| `features/terminal/api.ts` | 1004 | replace | 3 | 27 invokes and binary event decode become the in-process desktop-core bridge | open |
| `features/terminal/atlasFontSmoothing.ts` | 157 | retire | 3 | workaround for the xterm WebGL glyph atlas font smoothing | open |
| `features/terminal/atlasModelInvalidation.test.ts` | 147 | retire | 3 | tests the shared xterm WebGL atlas model invalidation | open |
| `features/terminal/atlasStaleProbe.test.ts` | 180 | retire | 3 | tests atlasStaleProbe.ts, which retires | open |
| `features/terminal/atlasStaleProbe.ts` | 176 | retire | 3 | detects a stale xterm WebGL shared atlas | open |
| `features/terminal/cellMetrics.ts` | 213 | replace | 3 | terminal/cell_metrics.rs: measures the shaped font; snapping rules and `cellsForBox` carried over | done (T3.3) |
| `features/terminal/clientSize.test.ts` | 208 | port | 3 | tests in terminal/client_size.rs | done (T3.3) |
| `features/terminal/clientSize.ts` | 112 | port | 3 | terminal/client_size.rs | done (T3.3) |
| `features/terminal/echoLagProbe.test.ts` | 334 | port | 3 | tests in terminal/input/latency.rs; the dispose and per-pane cases drop, since each pane owns its probe | done (T3.4) |
| `features/terminal/echoLagProbe.ts` | 272 | port | 3 | terminal/input/latency.rs (EchoLagProbe), one per pane in PaneInput; a journalled lag reaches the link quality verdict as LinkEvent::EchoLag | done (T3.4) |
| `features/terminal/glyphWeight.test.ts` | 231 | port | 3 | atlas canvas hook, xterm option tripwires and bundle-regex cases drop; cell-width snapping cases in terminal/cell_metrics.rs | done (T3.3) |
| `features/terminal/gridMismatchProbe.test.ts` | 165 | port | 3 | tests in terminal/probes.rs | done (T3.3) |
| `features/terminal/gridMismatchProbe.ts` | 160 | port | 3 | terminal/probes.rs (GridMismatchProbe) | done (T3.3) |
| `features/terminal/historyPage.test.ts` | 139 | port | 3 | tests in crates/terminal-engine/src/history_page.rs | done (T3.1) |
| `features/terminal/historyPage.ts` | 170 | port | 3 | crates/terminal-engine/src/history_page.rs | done (T3.1) |
| `features/terminal/hostColors.ts` | 17 | port | 3 | terminal/integration/colors.rs: sent at start and on every theme change | done (T3.6) |
| `features/terminal/inputLatencyStats.test.ts` | 159 | port | 3 | tests in terminal/input/latency.rs | done (T3.4) |
| `features/terminal/inputLatencyStats.ts` | 192 | port | 3 | terminal/input/latency.rs (InputLatency, one app-wide window flushed each minute) | done (T3.4) |
| `features/terminal/internalPathDrag.test.ts` | 168 | port | 3 | tests in terminal/integration/transfers.rs; gesture ids, expiry and the native lane retire with the DataTransfer | done (T3.6) |
| `features/terminal/internalPathDrag.ts` | 213 | port | 3 | InternalPathDrag in terminal/integration/transfers.rs, dropped on a pane in integration/mod.rs; the Explorer starts the drag in Phase 5 | done (T3.6) |
| `features/terminal/layout.test.ts` | 66 | port | 3 | tests in terminal/layout.rs | done (T3.3) |
| `features/terminal/layout.ts` | 67 | port | 3 | terminal/layout.rs | done (T3.3) |
| `features/terminal/measureBox.test.ts` | 174 | port | 3 | xterm internal-field canary and FitAddon comparison drop; `deviceSafeLineHeight` sweep and `cellsForBox` chrome arithmetic in terminal/cell_metrics.rs | done (T3.3) |
| `features/terminal/nativeDragDropCoordinator.ts` | 112 | replace | 3 | GPUI's `ExternalPaths` drop on each pane in terminal/integration/mod.rs | done (T3.6) |
| `features/terminal/openExternalUrl.test.ts` | 31 | port | 3 | the Tauri and browser-tab split has no equivalent; the http(s) admission `activatedTerminalUrl` adds is tested in terminal/reading/links.rs, and desktop-core tests its own validation | done (T3.5) |
| `features/terminal/openExternalUrl.ts` | 19 | replace | 3 | direct call into desktop-core external_links from terminal/reading/mod.rs; the browser-tab fallback for the dev harness goes | done (T3.5) |
| `features/terminal/osc52Clipboard.ts` | 52 | port | 3 | decoded by the engine's listener (crates/terminal-engine/src/terminal.rs), bounded and gated by the setting in terminal/integration/clipboard.rs, and written only for the host on screen | done (T3.6) |
| `features/terminal/paintTailProbe.test.ts` | 384 | retire | 3 | tests the xterm write-backlog fingerprints of paintTailProbe.ts; the replacement has its own tests in terminal/probes.rs | done (T3.3) |
| `features/terminal/paintTailProbe.ts` | 368 | replace | 3 | terminal/probes.rs (PaintTail): main-thread block and stopped frame clock fingerprints and the `render.paintSlow` incident; the xterm write-backlog fingerprints go | done (T3.3) |
| `features/terminal/panePaintGate.test.ts` | 118 | port | 3 | tests in terminal/paint_gate.rs | done (T3.3) |
| `features/terminal/panePaintGate.ts` | 127 | replace | 3 | terminal/paint_gate.rs: the explorer waits for the pane's first paint, armed once per mount; released from TerminalElement. Phase 5 is the caller | done (T3.3) |
| `features/terminal/panePaintOrdering.test.tsx` | 274 | port | 3 | explorer ordered behind pane paint | open |
| `features/terminal/rendererDrainBound.test.ts` | 129 | port | 3 | ? xterm write callbacks drop; if the engine keeps a hide drain, port: a drain that never completes is answered by a bound with only the generation actually applied, a late completion cannot advance the published watermark, a sealed writer refuses writes and says so (frozen-pane failure) | open |
| `features/terminal/rendererResizeOrdering.test.ts` | 140 | retire | 3 | answered: the engine serializes a history rewrite and a resize by construction (one command loop), so this test retires with xterm's write queue | done (T3.3) |
| `features/terminal/revealPayloadBudget.test.ts` | 112 | port | 3 | reveal payload cost over the Rust contract; reuses tmux-control directly | open |
| `features/terminal/revealRetry.test.ts` | 91 | port | 3 | tests in crates/terminal-engine/src/reveal_retry.rs | done (T3.1) |
| `features/terminal/revealRetry.ts` | 123 | port | 3 | crates/terminal-engine/src/reveal_retry.rs | done (T3.1) |
| `features/terminal/staleRestoreSilence.test.ts` | 533 | port | 3 | restore admission and splice refusal decisions | open |
| `features/terminal/terminalClientLifecycle.test.ts` | 162 | port | 3 | native client ownership | open |
| `features/terminal/terminalInputPolicy.test.ts` | 327 | port | 3 | translation and clipboard-alias cases in terminal/input/policy.rs (T3.4); copy-on-select cases in tests/terminal_reading.rs (T3.5), OSC 52 cases with T3.6 | partial (T3.4) |
| `features/terminal/terminalInputPolicy.ts` | 117 | port | 3 | terminal/input/policy.rs, with the keyboard encoding xterm.js did in terminal/input/keys.rs, mouse reports in mouse.rs and IME in ime.rs; `copyCompletedTerminalSelection` and `installTerminalCopyOnSelect` in terminal/reading/selection.rs and mod.rs (T3.5) | partial (T3.4) |
| `features/terminal/terminalLinkActivation.test.ts` | 13 | port | 3 | tests in terminal/reading/links.rs | done (T3.5) |
| `features/terminal/terminalLinkActivation.ts` | 9 | port | 3 | terminal/reading/links.rs (`is_link_activation`) | done (T3.5) |
| `features/terminal/terminalPanePerf.test.ts` | 154 | port | 3 | tests in terminal/probes.rs | done (T3.3) |
| `features/terminal/terminalPanePerf.ts` | 202 | port | 3 | terminal/probes.rs (PanePerf) | done (T3.3) |
| `features/terminal/terminalSeedOverlay.test.ts` | 59 | port | 3 | re-assert on the alacritty_terminal grid the seed overlay's erase cases: attributed erase fills through the right edge, erase keeps soft-wrap metadata, adjacent blank spans keep their own backgrounds (the S9 lane does not assert them by name) | open |
| `features/terminal/terminalTransferApi.test.ts` | 231 | port | 3 | native clipboard and event decode tests in terminal/integration/transfers.rs | done (T3.6) |
| `features/terminal/terminalTransferApi.ts` | 346 | replace | 3 | TerminalBackend::start_upload and cancel_upload in connection_ui/backend.rs, straight into desktop-core; only a digest-verified, published completion is pasted | done (T3.6) |
| `features/terminal/terminalTransferRegistry.test.tsx` | 154 | port | 3 | tests in terminal/integration/transfers.rs | done (T3.6) |
| `features/terminal/terminalTransferRegistry.ts` | 101 | port | 3 | TransferRecords in terminal/integration/transfers.rs, held app-wide by upload.rs; a finished record is never reopened, the full canonical merge is not ported | done (T3.6) |
| `features/terminal/terminalTransfers.test.ts` | 208 | port | 3 | tests in terminal/integration/transfers.rs; the canvas PNG encoder cases retire with the WebView | done (T3.6) |
| `features/terminal/terminalTransfers.ts` | 367 | port | 3 | terminal/integration/transfers.rs; the batch order and cancellation in upload.rs. The WebView image encoder retires: the native reader stages PNGs | done (T3.6) |
| `features/terminal/terminalViewport.ts` | 66 | port | 3 | ViewportAnchor in crates/terminal-engine/src/terminal.rs | done (T3.3) |
| `features/terminal/theme.test.ts` | 107 | port | 3 | behavior assertions move to the replacement of theme.ts | open |
| `features/terminal/theme.ts` | 223 | replace | 3 | reads --term-* CSS properties; becomes the Rust theme and generated palette | open |
| `features/terminal/timeBound.test.ts` | 63 | retire | 3 | tests timeBound.ts, which retires | done (T3.1) |
| `features/terminal/timeBound.ts` | 63 | retire | 3 | a promise race around calls that could wedge; the engine runs on deadlines and its writes cannot wedge | done (T3.1) |
| `features/terminal/topologyNotification.test.ts` | 29 | port | 3 | topology notification decode: cases in the shared `topology.json` fixture, run by `crates/terminal-interactions` and vitest | done (T3.2) |
| `features/terminal/viewportPreservation.test.ts` | 79 | port | 3 | tests in crates/terminal-engine/src/terminal.rs, on the engine grid | done (T3.3) |

### `features/terminal/testing`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/terminal/testing/RustPaneHost.ts` | 72 | retire | 3 | TS shim that spawns the tmux-control terminal_contract example; Rust tests call it in-process | open |

### `features/tmux`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/tmux/actionReconciliation.test.ts` | 122 | port | 4 | tests of actionReconciliation.ts | open |
| `features/tmux/actionReconciliation.ts` | 145 | port | 4 | tmux action reconciliation | open |
| `features/tmux/actions.test.ts` | 116 | port | 4 | tests of actions.ts | open |
| `features/tmux/actions.ts` | 195 | port | 4 | tmux actions | open |

### `features/transfers`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/transfers/transferState.test.ts` | 69 | port | 4 | tests of transferState.ts | open |
| `features/transfers/transferState.ts` | 190 | port | 4 | transfer state model | open |

### `features/workspaces`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/workspaces/TabStrip.tsx` | 548 | port | 2 | tab strip on Kit tab; pointer-drag source, in-app | open |
| `features/workspaces/WorkspaceSidebar.hosts.test.tsx` | 234 | port | 2 | multi-host sidebar rows | open |
| `features/workspaces/WorkspaceSidebar.tsx` | 973 | port | 2 | workspace sidebar on Kit sidebar; pointer-drag source, in-app | open |
| `features/workspaces/WorkspaceSwitcher.test.tsx` | 158 | port | 2 | `tests/commands.rs` | open |
| `features/workspaces/WorkspaceSwitcher.tsx` | 144 | port | 2 | `commands/quick_open.rs`; row and agent-state glyph and file-type icon land with Phases 4 and 5 | open |
| `features/workspaces/mergedWorkspaceRows.test.ts` | 109 | port | 4 | tests of mergedWorkspaceRows.ts | open |
| `features/workspaces/mergedWorkspaceRows.ts` | 103 | port | 4 | merged multi-host workspace rows | open |
| `features/workspaces/quickOpen.ts` | 53 | port | 2 | `commands/quick_open.rs` | open |
| `features/workspaces/transientDrag.test.tsx` | 101 | port | 2 | tests of transientDrag.ts | open |
| `features/workspaces/transientDrag.ts` | 71 | port | 2 | captured-pointer divider drag | open |
| `features/workspaces/useQuickOpenSearch.ts` | 48 | port | 2 | `commands/quick_open.rs` `sync_search`; root resolution belongs to the file-search source | open |
| `features/workspaces/workspaceRows.test.ts` | 223 | port | 4 | tests of workspaceRows.ts | open |
| `features/workspaces/workspaceRows.ts` | 215 | port | 4 | workspace rows | open |

### `perf`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `perf/bootstrap.ts` | 34 | replace | 3 | perf probe enablement becomes a direct desktop-core query | open |
| `perf/operations.ts` | 27 | port | 3 | operation counters | open |
| `perf/paintTicket.test.ts` | 20 | port | 3 | tests of paintTicket.ts | open |
| `perf/paintTicket.ts` | 61 | port | 3 | paint ticket | open |
| `perf/probe.test.ts` | 261 | port | 3 | tests of probe.ts | open |
| `perf/probe.ts` | 571 | port | 3 | perf probe keeping the perf-log schema | open |
| `perf/surfacePaint.test.ts` | 101 | port | 5 | the editor-generation case drops; port the paint-reporter cases: a superseded load keeps the successor's ticket, abandon needs a fresh commit, a stale lifetime publishes nothing | open |
| `perf/surfacePaint.ts` | 206 | replace | 5 | tracks which lazily mounted Monaco generation is ready; GPUI reports paint from the view | open |

### `startup`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `startup/editorPreload.test.ts` | 191 | retire | 5 | tests editorPreload.ts, which retires | open |
| `startup/editorPreload.ts` | 82 | retire | 5 | idle preload of the lazy Monaco chunk; no chunk to load | open |
| `startup/fontGate.test.ts` | 60 | retire | 2 | tests fontGate.ts, which retires | open |
| `startup/fontGate.ts` | 50 | retire | 2 | waits for a web font before xterm measures a cell; fonts are embedded so metrics exist at start | open |

### `state`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `state/connectionReducer.test.ts` | 226 | port | 2 | tests in connection_ui/connection_reducer.rs; ordered-event cases dropped with the dead arms | done (T2.4) |
| `state/connectionReducer.ts` | 194 | port | 2 | connection_ui/connection_reducer.rs; orderedSnapshot, orderedEvent and resync arms had no production dispatch and are not ported | done (T2.4) |
| `state/hostLinks.test.ts` | 173 | port | 2 | tests in connection_ui/host_links.rs | done (T2.4) |
| `state/hostLinks.ts` | 217 | port | 2 | connection_ui/host_links.rs | done (T2.4) |

### `transport`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `transport/abortable.ts` | 68 | replace | 5 | cancellation by tokio CancellationToken in desktop-core clients | open |

### `ui`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `ui/AgentStateIndicator.tsx` | 39 | port | 2 | agents/view.rs `state_indicator` | done (T4.2) |
| `ui/ContextMenu.tsx` | 211 | replace | 2 | Kit `PopupMenu` through `context_menu.rs`. Not carried over: opening from the keyboard, which Kit lacks | open |
| `ui/DelayedLoading.tsx` | 19 | port | 2 | loading text shown after 150 ms (the delay is a CSS animation in styles.css) | open |
| `ui/Icon.tsx` | 244 | port | 2 | icon path data becomes embedded SVG assets | open |
| `ui/SetupProgress.tsx` | 6 | port | 2 | setup progress line | open |
| `ui/StateDot.tsx` | 53 | port | 2 | agents/view.rs `state_indicator`, glyphs from `shell.agent_state_glyphs` | done (T4.2) |
| `ui/SurfaceError.tsx` | 50 | port | 2 | surface error with disclosure | open |
| `ui/appearance.test.ts` | 79 | port | 2 | theme preference and palette parity; palette cases become generator output tests | open |
| `ui/appearance.ts` | 130 | port | 2 | theme preference and UI scale over app_state | open |
| `ui/errorSummary.test.ts` | 109 | port | 2 | tests of errorSummary.ts | open |
| `ui/errorSummary.ts` | 142 | port | 2 | host rejection to sentence | open |

