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
| All | 341 files / 74,199 lines | 34 files / 9,835 lines | 28 files / 2,750 lines | 403 files / 86,784 lines |
| Production | 177 files / 36,198 lines | 34 files / 9,835 lines | 14 files / 1,029 lines | 225 files / 47,062 lines |
| Tests | 164 files / 38,001 lines | 0 files / 0 lines | 14 files / 1,721 lines | 178 files / 39,722 lines |

By target phase:

| Target phase | port | replace | retire | Total files |
|---|---|---|---|---|
| 2 | 91 | 10 | 5 | 106 |
| 3 | 86 | 11 | 13 | 110 |
| 4 | 84 | 5 | 1 | 90 |
| 5 | 74 | 8 | 8 | 90 |
| 7 | 6 | 0 | 0 | 6 |
| 9 | 0 | 0 | 1 | 1 |

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
| `monaco-languages.d.ts` | 7 | retire | 5 | Monaco language module typings; the Kit editor registers Tree-sitter grammars in editor/language.rs | done (T5.2) |
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
| `app/TerminalWorkspaceSurface.test.tsx` | 185 | port | 3 | separator rule and naming in terminal/layout.rs and surface.rs; divider rules in terminal/integration/frames.rs (T3.6), the grab's join rule in dragdrop/rules.rs and its gesture in workspaces/tests.rs (T4.3); key and pointer activity reports the pane through terminal/integration/agents.rs `note_activity`; the acknowledgement it drives is tested in agents/tests.rs (T4.2) | done (T4.2) |
| `app/TerminalWorkspaceSurface.tsx` | 193 | port | 3 | terminal/surface.rs: pane frames, resting dividers, empty state, region name, and (T3.6) divider drag and arrow-key resize, the attention ring (fed by the agents runtime in Phase 4), hover focus, selecting a pane in tmux on focus, and the link-modifier pane grab, a drag through dragdrop (T4.3) that joins a pane beside another; the grab yields to a link once T3.5's link test is wired into `integration::link_at` | done (T3.6) |
| `app/WorkspaceDock.test.tsx` | 131 | port | 2 | tests of WorkspaceDock.tsx | open |
| `app/WorkspaceDock.tsx` | 232 | port | 2 | dock layout on Kit dock; also a drop target, not a drag source (see S8) | open |
| `app/appRecovery.test.ts` | 43 | port | 2 | tests in connection_ui/app_recovery.rs | done (T2.4) |
| `app/appRecovery.ts` | 51 | port | 2 | connection_ui/app_recovery.rs | done (T2.4) |
| `app/bridgeFailureText.test.ts` | 13 | port | 2 | tests in connection_ui/controller.rs | done (T2.4) |
| `app/bridgeFailureText.ts` | 12 | port | 2 | connection_ui/controller.rs user_facing_bridge_failure | done (T2.4) |
| `app/editorChunkAbsence.test.tsx` | 225 | port | 5 | the chunk-not-fetched probe drops; binary file, file over 10 MiB and disconnected tab in editor/pane_tests.rs (T5.2); binary and oversized diff left to T5.3, Markdown preview-only to T5.4 | open |
| `app/editorChunkBoundary.test.tsx` | 386 | port | 5 | chunk evaluation order and the 500 ms preview publish delay drop; dirty draft surviving a reconnect in editor/document_tests.rs (T5.2), and the buffer lives in the document, not the editor view, so a preview switch keeps a pending save; the Live Markdown cases wait for Phase 7 | open |
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
| `app/shellNavigationCoordinator.ts` | 165 | port | 4 | workspaces/navigation.rs `Coordinator`: one move in flight, only the latest queued | done (T4.1) |
| `app/terminalFileOpenRoute.test.ts` | 36 | port | 3 | editor/open.rs `opens_only_while_the_pane_route_that_asked_still_holds` | done (T5.2) |
| `app/terminalFileOpenRoute.ts` | 27 | port | 3 | editor/open.rs `current_pane` | done (T5.2) |
| `app/types.ts` | 68 | replace | 2 | Session, Window, Pane, ConnectionSpec, HostProfile come from protocol and desktop-core types | open |
| `app/useAppAgentController.ts` | 162 | port | 5 | agents/mod.rs: scopes, focus and setup gating from the connection controller, rows across hosts (in the sidebar's own workspace order and pinned filter), the jump, row opening through the shell's `select_pane`, and the palette's row commands; the workflow half is agents/setup.rs; selecting a terminal tab acknowledges its pane (T4.5); bringing an agent's pane here and moving it are workspaces/moves.rs (T4.5). Phase 5: launch and resume need the active root (`agents::view::ActiveRoot`, set by the Explorer) | open |
| `app/useAppConnectionController.ts` | 944 | port | 2 | connection_ui/controller.rs; terminal hub and state cache, reseeds, echo-lag and input-latency probes, clipboard, file, git, agent and automation frame routing, retireConnection, appFocused and prewarm_terminal_bulk stay with Phase 3 and 4 owners; active-window resolution and the optimistic window with T4.1; after-install agent setup with useAgentHostSetup | done (T2.4) |
| `app/useAppFileActions.test.tsx` | 196 | port | 5 | tests of useAppFileActions.ts | open |
| `app/useAppFileActions.ts` | 145 | port | 5 | file actions entity (mutation, save panel, transfers) | open |
| `app/useAppHostSettingsActions.ts` | 179 | port | 4 | host connection, profile and helper actions in connection_ui/controller.rs (T2.4); the same actions now run from the Settings Connection tab (T4.4) | done (T2.4, T4.4) |
| `app/useAppRecoveryController.test.tsx` | 112 | port | 2 | connection_ui/controller/tests.rs | done (T2.4) |
| `app/useAppRecoveryController.ts` | 104 | port | 2 | connection_ui/controller.rs; T4.1 rewires its app-state access to the single in-memory owner and adds the flush on close | done (T2.4, T4.1) |
| `app/useAppShellChrome.test.tsx` | 91 | port | 2 | tests of useAppShellChrome.ts | open |
| `app/useAppShellChrome.ts` | 59 | port | 2 | rail layout and status-to-notice lifecycle | open |
| `app/useBulkTabClose.test.tsx` | 117 | port | 4 | set rules and the survivor report in workspaces/model.rs tests; the commit-time recheck in workspaces/tests.rs `closing_non_agent_tabs_keeps_a_window_that_may_hold_an_agent` (T4.6) | done (T4.1, T4.6) |
| `app/useBulkTabClose.ts` | 145 | port | 4 | workspaces/commands.rs `close_tab_set`, sets from workspaces/model.rs | done (T4.1) |
| `app/useClientResize.test.tsx` | 421 | port | 3 | tests in terminal/client_size.rs | done (T3.3) |
| `app/useClientResize.ts` | 320 | port | 3 | terminal/client_size.rs (ClientResize) driven by terminal/surface.rs | done (T3.3) |
| `app/useFocusHistoryNavigation.test.tsx` | 190 | port | 4 | workspaces/focus_history.rs tests and the Back end-to-end test in workspaces/tests.rs | done (T4.1) |
| `app/useFocusHistoryNavigation.ts` | 135 | port | 4 | workspaces/mod.rs `step_history`, `back_from_closing` | done (T4.1) |
| `app/useMissingHelperRecovery.ts` | 176 | port | 2 | connection_ui/helper_recovery.rs | done (T2.4) |
| `app/usePaneMoves.test.tsx` | 160 | port | 3 | workspaces/tests.rs (drops end to end on a fake host: the window on screen follows where the host says it landed, a covering document keeps the view, bring-here only from another tab, workspace choices) and `pane_on_screen_moved_by` | done (T4.3) |
| `app/usePaneMoves.ts` | 201 | port | 3 | workspaces/moves.rs: the drop plan against the host on screen, `Workspaces::run_plan`, the tab, pane and agent payloads, and the agent menu's moves (the pane grab of T3.6 now drags through dragdrop) | done (T4.3) |
| `app/usePerHostMemo.test.tsx` | 61 | retire | 2 | tests usePerHostMemo.ts, which retires | open |
| `app/usePerHostMemo.ts` | 56 | retire | 2 | React hook that memoizes a list per host; entities hold per-host state directly | open |
| `app/useShellNavigation.test.tsx` | 1378 | port | 4 | coordinator and write-planning cases in workspaces/navigation.rs, the optimistic window in connection_ui/host_links.rs, select and placeholder cases end to end in workspaces/tests.rs; React identity and rerender cases dropped | done (T4.1) |
| `app/useShellNavigation.ts` | 735 | port | 4 | workspaces/mod.rs `navigate`, `fly`, `land`, app-tab protection and the create placeholder | done (T4.1) |
| `app/useTerminalFileOpen.ts` | 108 | port | 3 | editor/open.rs `open_terminal_path`, called from terminal/reading/mod.rs for a file link | done (T5.2) |
| `app/useTmuxActionPerformer.test.tsx` | 187 | port | 4 | reconciliation cases in workspaces/actions.rs, the refusal and confirmation flows end to end in workspaces/tests.rs | done (T4.1) |
| `app/useTmuxActionPerformer.ts` | 146 | port | 4 | workspaces/mod.rs `perform`: the target's client and scope, silent or visible feedback, `action.refused` incident | done (T4.1) |
| `app/useWorkspaceCreate.test.tsx` | 142 | port | 4 | per-host defaults in workspaces/model.rs, the startup command end to end in workspaces/tests.rs | done (T4.1) |
| `app/useWorkspaceCreate.ts` | 69 | port | 4 | workspaces/mod.rs `create_workspace`, `send_startup` | done (T4.1) |
| `app/useWorkspaceDomainController.test.ts` | 27 | port | 5 | tmux half in T4.1: a tmux request needs the link's client, writes and server identity (workspaces/mod.rs `perform`). Phase 5: the file and Git scope half; the Git scope half in git/model.rs `git_scope`, tested in git/tests.rs (T5.3) | open |
| `app/useWorkspaceDomainController.ts` | 99 | port | 5 | tmux half in T4.1: the tmux request scope in workspaces/mod.rs `perform` and `scope_current`. Phase 5: the live file scope; the Git scope half in git/model.rs `git_scope` and git/mod.rs `sync` (T5.3) | open |
| `app/windowSelection.test.ts` | 79 | port | 4 | tests in workspaces/navigation.rs and connection_ui/host_links.rs | done (T4.1) |
| `app/windowSelection.ts` | 97 | port | 4 | workspaces/navigation.rs; the optimistic window guard in connection_ui/host_links.rs `settle_active_window` | done (T4.1) |

### `commands`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `commands/CommandPalette.tsx` | 126 | port | 2 | `commands/palette.rs`: own list, not Kit `Command`, whose virtualized rows leave offscreen commands out of the accessibility tree | open |
| `commands/ConfirmationDialog.tsx` | 49 | replace | 2 | Kit alert dialog, `commands/dialogs.rs`. Cancel holds focus, so Enter cancels. Non-tmux `busy` state is not needed yet | open |
| `commands/NewWorkspaceDialog.test.tsx` | 118 | port | 4 | `default_host` tests in workspaces/dialog.rs; overlay isolation and Escape are Kit's | done (T4.1) |
| `commands/NewWorkspaceDialog.tsx` | 113 | port | 4 | workspaces/dialog.rs on `open_overlay` | done (T4.1) |
| `commands/ShortcutEditorDialog.test.tsx` | 43 | port | 2 | `tests/commands.rs` | open |
| `commands/ShortcutEditorDialog.tsx` | 53 | port | 2 | `commands/shortcut_editor.rs` | open |
| `commands/TextInputDialog.tsx` | 34 | replace | 2 | Kit dialog, `commands/dialogs.rs`; Enter does not submit while composing | open |
| `commands/destructiveConfirmation.test.ts` | 84 | port | 2 | title and detail in `tests/commands.rs`; the action checks in workspaces/actions.rs tests | done (T4.1) |
| `commands/destructiveConfirmation.ts` | 45 | port | 2 | title and detail in `commands/dialogs.rs`; the action checks in workspaces/actions.rs `is_destructive` and workspaces/commands.rs `close` | done (T4.1) |
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
| `diagnostics/copyChordJournal.test.ts` | 44 | port | 3 | tests in `diagnostics.rs` | done (T4.4) |
| `diagnostics/copyChordJournal.ts` | 37 | port | 3 | `diagnostics.rs` `CopyChordJournal`, fed from `commands::on_keystroke` on Linux | done (T4.4) |
| `diagnostics/incidents.test.ts` | 47 | port | 2 | record format cases in `diagnostics.rs`, including a detail that cannot overwrite `kind`, `t` or `launch`; the write-failure case is the backend's `let _ =` | done (T4.4) |
| `diagnostics/incidents.ts` | 39 | replace | 2 | direct call into desktop-core incidents (renderer half of incidents.rs); the record is built by `diagnostics::incident_record` | done (T4.4) |

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
| `features/agents/sound.ts` | 70 | replace | 4 | agents/sound.rs: the same two cues synthesized and played with rodio, preferences read from `settings::get` when a cue plays (T4.5) | done (T4.2, T4.5) |
| `features/agents/state.test.ts` | 233 | port | 4 | tests in agents/model.rs | done (T4.2) |
| `features/agents/state.ts` | 153 | port | 4 | agents/model.rs `AgentStore` | done (T4.2) |
| `features/agents/testFixtures.ts` | 20 | port | 4 | agents/model.rs `fixtures` | done (T4.2) |
| `features/agents/types.ts` | 293 | port | 4 | agents/model.rs | done (T4.2) |
| `features/agents/useAgentHostSetup.test.tsx` | 624 | port | 4 | prompt, decline, install, frozen state, helper auto-setup and reassert cases in agents/tests.rs; host binding through the `consented_scope` refusal there (a switch between review and apply takes the same check) | done (T4.2) |
| `features/agents/useAgentHostSetup.tsx` | 448 | port | 4 | agents/setup.rs | done (T4.2) |
| `features/agents/useAgentNotificationActivation.test.tsx` | 115 | port | 4 | route and resolution cases in agents/activation.rs; the stale-error text match is replaced by a wait for the pane to reach the screen (T4.5); the focus and exact acknowledgement flow is checked in QA | done (T4.2, T4.5) |
| `features/agents/useAgentNotificationActivation.ts` | 146 | port | 4 | agents/activation.rs: clicks are queued off the notification thread and polled; the pane is put on screen by the shell's `select_pane`, and a pane that does not arrive in time is resolved once more over a fresh connection (T4.5) | done (T4.2, T4.5) |
| `features/agents/useAgentRuntime.test.tsx` | 505 | port | 4 | tests in agents/tests.rs against a scripted host | done (T4.2) |
| `features/agents/useAgentRuntime.ts` | 378 | port | 4 | agents/runtime.rs; the topology authority (the tmux window set each requested snapshot was asked against, current while the scope's topology generation is) is `AgentRuntime::topology_authority`, which agents/mod.rs `agent_shell_from` turns into `AgentPresence::Absent` for the bulk tab close (T4.6) | done (T4.2, T4.6) |
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
| `features/dragDrop/DragGhost.tsx` | 11 | port | 4 | dragdrop/mod.rs `ghost`, dimmed over a refusal | done (T4.3) |
| `features/dragDrop/dropRules.test.ts` | 163 | port | 4 | tests in dragdrop/rules.rs | done (T4.3) |
| `features/dragDrop/dropRules.ts` | 158 | port | 4 | dragdrop/rules.rs `plan_drop` | done (T4.3) |
| `features/dragDrop/pointerDrag.test.ts` | 190 | port | 4 | tests in dragdrop/tests.rs, and the gestures end to end in workspaces/tests.rs | done (T4.3) |
| `features/dragDrop/pointerDrag.ts` | 277 | port | 4 | dragdrop/mod.rs: window-level mouse listeners and targets that offer themselves under the pointer, instead of DOM pointer capture and a geometry hit test; no grabbing cursor while dragging | done (T4.3) |

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
| `features/files/FileEditor.tsx` | 57 | replace | 5 | editor/pane.rs: the Kit code editor (gpui-component `Editor`) with the toolbar, find bar and go-to box | done (T5.2) |
| `features/files/LiveMarkdownEditor.test.tsx` | 117 | port | 7 | tests of LiveMarkdownEditor.tsx | open |
| `features/files/LiveMarkdownEditor.tsx` | 154 | port | 7 | ? depends on S4: does Live Markdown fit the Kit editor or gpui-base text | open |
| `features/files/api.test.ts` | 506 | port | 5 | behavior assertions move to the replacement of api.ts | open |
| `features/files/api.ts` | 647 | replace | 5 | Tauri file client becomes direct desktop-core calls; its request validation moves with it | open |
| `features/files/autosave.test.ts` | 104 | port | 5 | editor/autosave.rs tests, and the document-level cases in editor/document_tests.rs | done (T5.2) |
| `features/files/autosave.ts` | 108 | port | 5 | editor/autosave.rs `Autosave`; driven by editor/document.rs | done (T5.2) |
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
| `features/files/editorFlushRegistry.test.ts` | 27 | port | 5 | editor/document_tests.rs `flushing_every_editor_waits_for_each_save_and_fails_if_one_does` | done (T5.2) |
| `features/files/editorFlushRegistry.ts` | 26 | port | 5 | editor/mod.rs `flush_all` over mounted and retiring documents; gates window close, app tab close and bulk close | done (T5.2) |
| `features/files/editorLanguage.test.ts` | 31 | port | 5 | editor/language.rs tests; `every_grammar_highlights` checks each grammar colors a sample | done (T5.2) |
| `features/files/editorLanguage.ts` | 34 | port | 5 | editor/language.rs `language_for_path` over eleven Tree-sitter grammars | done (T5.2) |
| `features/files/editorLayout.test.tsx` | 118 | retire | 5 | tests editorLayout.ts, which retires | done (T5.2) |
| `features/files/editorLayout.ts` | 99 | retire | 5 | forces Monaco to re-measure its container; no equivalent layout step in GPUI | done (T5.2) |
| `features/files/editorText.ts` | 18 | replace | 5 | the theme mono font and the Kit editor line height | done (T5.2) |
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
| `features/files/monaco.ts` | 152 | replace | 5 | theme/mod.rs `syntax()` builds the Kit highlight theme from the palette; Monaco workers, loader and font remeasure drop. The diff washes go with the diff view (T5.3) | done (T5.2) |
| `features/files/monarch.test.ts` | 93 | port | 5 | editor/language.rs Python and TypeScript capture tests | done (T5.2) |
| `features/files/monarch.ts` | 109 | port | 5 | editor/language.rs highlight query additions for Python and TypeScript | done (T5.2) |
| `features/files/saveStatus.test.tsx` | 229 | port | 5 | editor/save_status.rs tests | done (T5.2) |
| `features/files/saveStatus.ts` | 51 | port | 5 | editor/save_status.rs | done (T5.2) |
| `features/files/searchCount.test.ts` | 79 | port | 5 | editor/find.rs tests | done (T5.2) |
| `features/files/searchCount.ts` | 85 | port | 5 | editor/find.rs `count_label`, with the find, replace and go-to-line matching | done (T5.2) |
| `features/files/types.ts` | 280 | port | 5 | file types, limits, wire shapes | open |
| `features/files/useActiveRoot.ts` | 280 | port | 5 | active root resolution; waits on pane paint gate | open |
| `features/files/useConnectionTransfers.ts` | 85 | port | 5 | per-connection download transfers | open |
| `features/files/useOpenFileTab.ts` | 504 | port | 5 | editor/tabs.rs `open_file_tab` (preview-tab rule), editor/open.rs (explorer, terminal path and Quick Open opens), editor/document.rs (read, watch reconciliation, reload rules) | done (T5.2) |
| `features/files/useWorkspaceFiles.test.tsx` | 1402 | port | 5 | tests of useWorkspaceFiles.ts | open |
| `features/files/useWorkspaceFiles.ts` | 908 | port | 5 | workspace files controller | open |
| `features/files/watchLeases.test.ts` | 208 | port | 5 | tests of watchLeases.ts | open |
| `features/files/watchLeases.ts` | 181 | port | 5 | watch leases | open |
| `features/files/webkitWidgetBuffers.test.ts` | 43 | retire | 5 | CodeMirror widget buffer behavior in WebKit | done (T5.2) |
| `features/files/wire.ts` | 135 | port | 5 | file wire decode and transfer state validation | open |

### `features/git`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/git/GitCommitForm.tsx` | 166 | port | 5 | git/panel.rs: the commit form (`render_form`, `run`), with Commit, Commit & Push and Push | done (T5.3) |
| `features/git/GitDiffEditor.test.tsx` | 39 | retire | 5 | asserts Monaco diff option values through a mocked wrapper; wrap and alignment are covered by the Phase 5 diff exit gate | done (T5.3) |
| `features/git/GitDiffEditor.tsx` | 60 | replace | 5 | Kit Diff (read-only, virtualized) with a whole-file patch from similar (git/model.rs `whole_file_patch`), wrapped, side by side from 900 px and inline below, in git/diff_view.rs | done (T5.3) |
| `features/git/GitDiffSurface.test.tsx` | 350 | port | 5 | logic cases in git/tests.rs (the diff tab); drawing-only cases are covered by the T5.3 and Phase 5 QA | done (T5.3) |
| `features/git/GitDiffSurface.tsx` | 237 | port | 5 | git/diff_view.rs, reached through `git::diff_surface`, the document area's route for a Git diff tab | done (T5.3) |
| `features/git/GitSidebar.test.tsx` | 588 | port | 5 | logic cases in git/tests.rs (the panel and its model); drawing-only cases (row layout, drag payload, DOM bounds) are covered by the T5.3 and Phase 5 QA | done (T5.3) |
| `features/git/GitSidebar.tsx` | 450 | port | 5 | git/panel.rs; the row's drag carries `InternalPathDrag` (see S8) | done (T5.3) |
| `features/git/api.test.ts` | 169 | port | 5 | tests in git/tests.rs (the wire) | done (T5.3) |
| `features/git/api.ts` | 559 | replace | 5 | git/client.rs over desktop-core through the connection backend; the wire shapes in git/model.rs | done (T5.3) |
| `features/git/repositoryStore.test.ts` | 306 | port | 5 | tests in git/tests.rs (the shared observation) | done (T5.3) |
| `features/git/repositoryStore.ts` | 470 | port | 5 | git/store.rs | done (T5.3) |
| `features/git/types.ts` | 144 | port | 5 | git/model.rs | done (T5.3) |
| `features/git/useSharedGitDiff.ts` | 268 | port | 5 | git/diff_view.rs | done (T5.3) |
| `features/git/useWorkspaceGit.test.tsx` | 163 | port | 5 | tests in git/tests.rs (the panel's observation) | done (T5.3) |
| `features/git/useWorkspaceGit.ts` | 78 | port | 5 | git/panel.rs `set_context` and git/mod.rs `sync` | done (T5.3) |

### `features/ports`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/ports/PortsPanel.test.tsx` | 118 | port | 4 | tests in ports/tests.rs against a fake transport | done (T4.3) |
| `features/ports/PortsPanel.tsx` | 162 | port | 4 | ports/view.rs and ports/panel.rs, the right panel's "ports" segment | done (T4.3) |
| `features/ports/api.ts` | 38 | replace | 4 | direct desktop-core calls in ports/panel.rs (`NativePorts`, polled on a timer); parse_port in ports/model.rs | done (T4.3) |
| `features/ports/usePortForwards.ts` | 37 | port | 4 | ports/panel.rs `PortsPanel` | done (T4.3) |

### `features/shell`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/shell/AppTabSurface.test.tsx` | 644 | port | 5 | read, reload, echo, rescan, bootstrap and save-status cases in editor/document_tests.rs and editor/save_status.rs; T5.2 left the Markdown link case to T5.4 and the watch lease release to the T5.1 watcher | open |
| `features/shell/AppTabSurface.tsx` | 366 | port | 5 | editor/mod.rs `surface` routes document, diff (T5.3 `set_diff_surface`) and preview (T5.4 `set_markdown_preview`); editor/pane.rs draws the document tab | done (T5.2) |
| `features/shell/DisconnectedStrip.tsx` | 75 | port | 2 | connection_ui/surface.rs | done (T2.4) |
| `features/shell/MarkdownPreview.test.tsx` | 346 | port | 5 | ? preview keeps selection while content updates; DOM selection mechanics may not apply, keep the behavior | open |
| `features/shell/RightPanel.tsx` | 120 | port | 2 | right panel container with resizable width | open |
| `features/shell/SettingsDialog.tsx` | 611 | port | 4 | `settings/dialog.rs` with the live values in `settings/prefs.rs` and `settings/mod.rs`. The terminal screen-reader setting is the agreed exception. Agent setup and the notifier are published by T4.2 through `settings::set_agent_setup` and `settings::set_notifier` | done (T4.4) |
| `features/shell/TitleBar.test.tsx` | 57 | port | 2 | tests of TitleBar.tsx | open |
| `features/shell/TitleBar.tsx` | 206 | port | 2 | title bar on GPUI client decorations | open |
| `features/shell/appStateContract.test.ts` | 334 | port | 4 | the fixture stays with desktop-core app_state.rs `app_state_contract_matches_the_frontend_shape`; the TS key-set cases retire with the TS app; normalize and clamp cases in ui/chrome/layout.rs and settings/prefs.rs tests. Not ported: the 1,024-entry cap on workspace defaults (desktop-core refuses a larger state on load; reaching it takes over 1,024 hosts) | done (T4.1, T4.4, T4.6) |
| `features/shell/appStatePersistence.test.ts` | 69 | port | 2 | connection_ui/controller/tests.rs (failed save keeps the change) and connection_ui/backend.rs (a late earlier save cannot undo a later one) | done (T4.1) |
| `features/shell/appStatePersistence.ts` | 65 | port | 2 | connection_ui/controller.rs chained saves and the quit flush; connection_ui/backend.rs numbered partial saves | done (T4.1) |
| `features/shell/focusHistory.test.ts` | 98 | port | 4 | tests in workspaces/focus_history.rs | done (T4.1) |
| `features/shell/focusHistory.ts` | 125 | port | 4 | workspaces/focus_history.rs | done (T4.1) |
| `features/shell/helperUpgrade.test.ts` | 78 | port | 2 | tests in connection_ui/helper_upgrade.rs | done (T2.4) |
| `features/shell/helperUpgrade.ts` | 89 | port | 2 | connection_ui/helper_upgrade.rs | done (T2.4) |
| `features/shell/hostLatency.test.ts` | 39 | port | 2 | tests in workspaces/tests.rs | done (T4.1) |
| `features/shell/hostLatency.ts` | 72 | port | 2 | workspaces/mod.rs `smoothed_latency`, shown on the sidebar host row | done (T4.1) |
| `features/shell/hostProfiles.test.ts` | 30 | port | 2 | tests in connection_ui/host_profiles.rs | done (T2.4) |
| `features/shell/hostProfiles.ts` | 31 | port | 2 | connection_ui/host_profiles.rs | done (T2.4) |
| `features/shell/hostScope.test.ts` | 43 | port | 2 | tests in connection_ui/host_scope.rs | done (T2.4) |
| `features/shell/hostScope.ts` | 47 | port | 2 | connection_ui/host_scope.rs | done (T2.4) |
| `features/shell/model.test.ts` | 806 | port | 4 | tests in workspaces/model.rs; file and Git cases with Phase 5 | done (T4.1) |
| `features/shell/model.ts` | 1018 | port | 4 | workspaces/model.rs; file and Git tabs with Phase 5 | done (T4.1) |
| `features/shell/panelExtensions.ts` | 48 | port | 4 | ui/chrome/right_panel.rs `PanelBodies::register` with layout.rs `panel_surfaces`; the host and events in automations/panel.rs `PanelHost` and `PanelEvent`; the toggle in settings/prefs.rs `set_panel_extension` | done (T4.2, T4.4) |
| `features/shell/responsiveShell.test.ts` | 86 | port | 2 | tests of responsiveShell.ts | open |
| `features/shell/responsiveShell.ts` | 67 | port | 2 | responsive rail layout | open |
| `features/shell/shellComponents.test.tsx` | 2301 | port | 4 | logic cases in agents/list.rs, workspaces/rows.rs, workspaces/model.rs, workspaces/tests.rs, dragdrop/rules.rs, commands/registry.rs and tests/settings.rs; marks, classes and CSS are view behavior, checked by T4.2 and T4.5 QA. Elsewhere: Download and document tabs with Phase 5, the update pill with `useUpdateCheck.ts`, All-tabs shortcut glyphs as on `TabStrip.tsx`, title-bar controls with `TitleBar.tsx`; Phase 5, with the document tabs: tests for menu commands kept on the connection that opened the menu (the menus capture that scope in their target) and for manual tab scrolling through background updates | done (T4.1, T4.2, T4.3, T4.5) |
| `features/shell/statusNotice.test.ts` | 94 | port | 2 | `notices.rs` unit tests | done (T2.4, T2.5) |
| `features/shell/statusNotice.ts` | 96 | port | 2 | `notices.rs`; one notice at a time, a problem stays until dismissed | done (T2.4, T2.5) |
| `features/shell/types.ts` | 458 | port | 4 | storage in desktop-core app_state.rs; clamps and fallbacks in ui/chrome/layout.rs `Prefs::from_shell` and settings/prefs.rs `Prefs::from_state`; the renamed agent orderings as serde aliases. Values serde refuses could never be saved, so their TS fallbacks retire | done (T4.1, T4.4) |
| `features/shell/useAppearance.ts` | 33 | port | 2 | `settings::update` applies a changed theme or size through `theme::apply`; `Shell` follows the OS while the choice is System | done (T4.4) |
| `features/shell/useDesktopResumeRecovery.test.ts` | 81 | port | 2 | tests in connection_ui/resume_recovery.rs | done (T2.4) |
| `features/shell/useDesktopResumeRecovery.ts` | 151 | port | 2 | connection_ui/resume_recovery.rs; the browser online event is polled from getifaddrs on the 5 s check | done (T2.4) |
| `features/shell/usePersistedAppState.test.tsx` | 89 | retire | 4 | tests only the localStorage sound import, which retires with the WebView (as `sound.test.ts`); app state has held the value since that import shipped | done (T4.6) |
| `features/shell/usePersistedAppState.ts` | 123 | port | 2 | persisted app state loader | open |
| `features/shell/useShellCommands.test.tsx` | 384 | port | 2 | dispatch in `tests/commands.rs`; each command body tested with its feature; T4.1's end to end in workspaces/tests.rs | open |
| `features/shell/useShellCommands.ts` | 443 | port | 2 | `commands/mod.rs` `run` and `on_command`: each command body lands with its feature (Phases 3 to 5); workspace, tab, window, pane, focus and dock bodies in workspaces/commands.rs (T4.1) | open |
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
| `features/terminal/terminalTransferRegistry.ts` | 101 | port | 3 | TransferRecords in terminal/integration/transfers.rs, held app-wide by upload.rs; a finished record is never reopened, and a late frame merges as the canonical state does (T4.3); frames are not validated on receipt, `transfers::validate` waits for Phase 5's download decode | done (T3.6, T4.3) |
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
| `features/tmux/actionReconciliation.test.ts` | 122 | port | 4 | tests in workspaces/actions.rs | done (T4.1) |
| `features/tmux/actionReconciliation.ts` | 145 | port | 4 | workspaces/actions.rs `request_reconciled` | done (T4.1) |
| `features/tmux/actions.test.ts` | 116 | port | 4 | tests in workspaces/actions.rs | done (T4.1) |
| `features/tmux/actions.ts` | 195 | port | 4 | workspaces/actions.rs | done (T4.1) |

### `features/transfers`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/transfers/transferState.test.ts` | 69 | port | 4 | tests in transfers.rs | done (T4.3) |
| `features/transfers/transferState.ts` | 190 | port | 4 | transfers.rs, shared by the terminal upload list (terminal/integration/transfers.rs, which merges late frames as the Tauri state does); `validate` is for Phase 5's download decode | done (T4.3) |

### `features/workspaces`

| File | Lines | Disposition | Phase | Target or reason | Status |
|---|---|---|---|---|---|
| `features/workspaces/TabStrip.tsx` | 548 | port | 2 | ui/chrome/tab_strip.rs; a terminal tab draws its agent's adapter glyph and `state_indicator` (glyphs from `settings::get`), and selecting it acknowledges its active pane (T4.5); a tab is a drag source and a drop target (T4.3), Download… and document tooltips with Phase 5; not yet: the per-tab tooltip, the All tabs menu's shortcut glyphs, and opening a tab's menu from the keyboard (no Kit support) | done (T4.1, T4.3, T4.5) |
| `features/workspaces/WorkspaceSidebar.hosts.test.tsx` | 234 | port | 2 | host letters and moves in workspaces/rows.rs, the host row in connection_ui/controller/tests.rs, rows end to end in workspaces/tests.rs | done (T4.1) |
| `features/workspaces/WorkspaceSidebar.tsx` | 973 | port | 2 | ui/chrome/sidebar.rs; the Agents section is `agents::view::AgentsList`, the rows' agent lines, the compact cluster and the state indicator use T4.2's marks through `AgentShell` (T4.5); a workspace row is a drop target (T4.3); not yet: phase dots in the host menu | done (T4.1, T4.3, T4.5) |
| `features/workspaces/WorkspaceSwitcher.test.tsx` | 158 | port | 2 | `tests/commands.rs` | open |
| `features/workspaces/WorkspaceSwitcher.tsx` | 144 | port | 2 | `commands/quick_open.rs`; workspace and tab rows from workspaces/commands.rs `Source` (T4.1); agent-state glyph and file-type icon land with T4.2 and Phase 5 | open |
| `features/workspaces/mergedWorkspaceRows.test.ts` | 109 | port | 4 | tests in workspaces/rows.rs | done (T4.1) |
| `features/workspaces/mergedWorkspaceRows.ts` | 103 | port | 4 | workspaces/rows.rs | done (T4.1) |
| `features/workspaces/quickOpen.ts` | 53 | port | 2 | `commands/quick_open.rs` | open |
| `features/workspaces/transientDrag.test.tsx` | 101 | port | 2 | tests of transientDrag.ts | open |
| `features/workspaces/transientDrag.ts` | 71 | port | 2 | captured-pointer divider drag | open |
| `features/workspaces/useQuickOpenSearch.ts` | 48 | port | 2 | `commands/quick_open.rs` `sync_search`; the search key, root cache and search in editor/open.rs `search_key` and `search_files` | done (T5.2) |
| `features/workspaces/workspaceRows.test.ts` | 223 | port | 4 | tests in workspaces/rows.rs | done (T4.1) |
| `features/workspaces/workspaceRows.ts` | 215 | port | 4 | workspaces/rows.rs | done (T4.1) |

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
| `startup/editorPreload.test.ts` | 191 | retire | 5 | tests editorPreload.ts, which retires | done (T4.4) |
| `startup/editorPreload.ts` | 82 | retire | 5 | idle preload of the lazy Monaco chunk; no chunk to load | done (T4.4) |
| `startup/fontGate.test.ts` | 60 | retire | 2 | tests fontGate.ts, which retires | done (T4.4) |
| `startup/fontGate.ts` | 50 | retire | 2 | waits for a web font before xterm measures a cell; fonts are embedded so metrics exist at start | done (T4.4) |

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
| `ui/StateDot.tsx` | 53 | port | 2 | agents/view.rs `state_indicator`, glyphs from `settings::get`; the sidebar rows and the tab strip draw it too (T4.5) | done (T4.2, T4.5) |
| `ui/SurfaceError.tsx` | 50 | port | 2 | surface error with disclosure; the editor save error draws it in editor/pane.rs `error_banner` (T5.2), other surfaces still to port | open |
| `ui/appearance.test.ts` | 79 | port | 2 | theme preference and palette parity; palette cases become generator output tests; the System, Light and Dark resolution cases are in `theme::tests` | done (T2.1, T4.4) |
| `ui/appearance.ts` | 130 | port | 2 | theme preference and UI scale over app_state: `theme::apply`, `settings::update`. The Markdown palette choice is `Prefs::markdown_appearance`, read by the Markdown view in Phase 5 | done (T2.1, T4.4) |
| `ui/errorSummary.test.ts` | 109 | port | 2 | ui/error_summary.rs tests | done (T5.2) |
| `ui/errorSummary.ts` | 142 | port | 2 | ui/error_summary.rs `summarize` | done (T5.2) |

