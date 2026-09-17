# Polish Round 1 — Technical Plan

Scope: one round of UI/UX polish from a hands-on review, plus a hook bug hit while
running Claude Code outside tmux and two terminal-reliability defects (pane sizing,
paused-pane stall) root-caused live against the user's remote host. Every item below states the **root cause** (with
file:line references verified against the current tree) and a **step-by-step fix** a
junior engineer can follow without re-deriving the investigation.

Reference target for all theming work — the user's Ghostty, resolved from
`~/.config/ghostty/config` + `ghostty +show-config`:

| Setting | Value |
| --- | --- |
| Font | Ghostty's bundled default: **JetBrains Mono Regular (400)**, no custom family |
| `font-size` | 13 |
| `font-thicken` | false (no stem darkening) |
| `bold-is-bright` | false (Ghostty default; SGR bold does **not** switch to bright colors) |
| Theme | **Ghostty Default Style Dark** |
| background / foreground | `#282c34` / `#ffffff` |
| cursor / cursor-text | `#ffffff` / `#353a44` |
| selection bg / fg | `#ffffff` / `#282c34` |
| ANSI 0–7 | `#1d1f21 #cc6566 #b6bd68 #f0c674 #82a2be #b294bb #8abeb7 #c4c8c6` |
| ANSI 8–15 | `#666666 #d54e53 #b9ca4b #e7c547 #7aa6da #c397d8 #70c0b1 #eaeaea` |

Good news discovered during investigation: the in-app terminal palette is **already**
Ghostty Default Style Dark, verbatim (`apps/desktop/src/tokens.css:79-101`, mirrored in
`features/terminal/theme.ts:17-40` and enforced by `theme.test.ts`). Items 1–3 are
therefore about the font rendering and the *chrome*, not the terminal colors.

Suggested implementation order (independent tracks, roughly by risk):

1. Track A — quick wins: Item 10 (Cmd+L), Item 9 (Cmd+T focus), Item 7 (settings select), Item 17 (hook fix), Item 19 (toast noise), Item 20 (close confirmation)
2. Track B — theme: Items 1, 2, 3 (one PR; they share tokens)
3. Track C — sidebar/agents: Items 4, 5, 6
4. Track D — explorer: Items 11, 12, 13, 14 (icons/font first, filtering second, preview tabs last)
5. Track E — downloads: Item 16 (picker-first flow, completion actions)
6. Track F — notifications: Item 8 (largest; touches Rust on 3 platforms)
7. Track G — terminal reliability: Items 18 (pane sizing) and 21 (paused-pane stall); both verified against the live remote host

---

## 1. Terminal font renders semi-bold (should match Ghostty)

**Symptom.** The terminal font looks heavier than Ghostty on the same machine, though
both use JetBrains Mono Regular at 13px.

**Root cause.** It is *not* the font file. The app bundles genuine
JetBrainsMono-Regular.woff2 (usWeightClass 400, static, verified) at
`apps/desktop/src/tokens.css:22-49`, mounting is gated on all four faces loading
(`apps/desktop/src/main.tsx:45-69`), so a Menlo/SF-Mono fallback is effectively
impossible. Two rendering-level causes:

1. **Canvas atlas rasterization bypasses font smoothing settings.** The terminal is
   xterm.js v6 with the WebGL addon (`features/terminal/TerminalRenderer.ts:5`,
   `:801-817`). Glyphs are rasterized into a 2D-canvas glyph atlas. The app never sets
   xterm's `allowTransparency`, which defaults to `false`, and the addon passes it
   straight into `getContext("2d", { alpha: allowTransparency })`. An **opaque** canvas
   on macOS WKWebView gets CoreGraphics subpixel/LCD smoothing **with stem darkening**
   baked into the atlas. The chrome's `-webkit-font-smoothing: antialiased`
   (`apps/desktop/src/styles.css:28`) is a DOM-text property and never reaches canvas.
   Ghostty rasterizes through its own Metal pipeline with `font-thicken = false`. Net:
   same face, visibly heavier stems in the app.
2. **`drawBoldTextInBrightColors` defaults to `true`** in xterm and is never overridden
   (the options object at `TerminalRenderer.ts:359-397` sets no weight-related options).
   Ghostty's `bold-is-bright` is false, so SGR-bold text in the app is double-emphasized
   (bold face **and** bright color) relative to Ghostty.

**Fix — root cause now PROVEN by a WebKit probe (see "Probe evidence" below); the
earlier `allowTransparency` hypothesis was tested and REFUTED.**

Probe evidence (Playwright WebKit 18.2, canvas replicating the addon's exact atlas
path — `getContext("2d", { alpha, willReadFrequently: true })`, background fill,
`fillText`, ink-mass measured per text band; DOM reference = same font with
`-webkit-font-smoothing: antialiased`):

| Case | Ink mass | Verdict |
| --- | --- | --- |
| DOM text, antialiased (target) | 504.8k | reference |
| canvas `alpha: false` (app today) | 665.5k | **+32% heavier — the bug** |
| canvas `alpha: true` | 665.5k | identical → `allowTransparency` fixes nothing |
| detached canvas + inline `-webkit-font-smoothing` style | 665.5k | inline style on a detached element is ignored |
| **canvas attached to the document + `-webkit-font-smoothing: antialiased`** | **504.8k** | **exact match with the DOM reference** |

So: WebKit's canvas `fillText` honors the canvas **element's computed**
`-webkit-font-smoothing`, computed style only exists for elements **in the document**,
and the WebGL addon creates its glyph-atlas canvas via `document.createElement`
**detached** — which is why the app's existing `:root { -webkit-font-smoothing:
antialiased }` (`styles.css:28`) never reaches terminal glyphs.

1. Add a small module (e.g. `features/terminal/atlasFontSmoothing.ts`), imported before
   any `WebglAddon` is activated, that installs a targeted hook on
   `HTMLCanvasElement.prototype.getContext`: when a **2d** context is requested with
   `willReadFrequently: true` (the atlas's signature) on a canvas that is **not
   connected** to the document, first append that canvas to a hidden off-screen holder
   (`position: absolute; left: -9999px; top: 0`) that carries
   `-webkit-font-smoothing: antialiased`. Then call the original `getContext`.
   - Must be a persistent hook, not a one-shot during addon activation: the addon
     recreates atlas pages lazily (font changes, `clearTextureAtlas`,
     `TerminalRenderer.ts:787-799`).
   - Use the exact probe-proven holder styling (`left: -9999px`, *not*
     `display: none` — that is the configuration the probe validated).
   - Document why in the module header, linking the probe numbers above.
2. In the `new Terminal({...})` options (`TerminalRenderer.ts:359-397`) add
   `drawBoldTextInBrightColors: false` (Ghostty's `bold-is-bright` default) and pin
   `fontWeight: "normal"`, `fontWeightBold: "bold"`. Do **not** set
   `allowTransparency` — refuted above.
3. Keep `minimumContrastRatio` untouched (default 1 — Ghostty does no contrast
   adjustment either).
4. QA (parallel-safe, no app needed): adapt the probe at
   `<scratchpad>/fontprobe/probe3.html` — load the repo's bundled xterm + WebGL addon +
   the new hook, screenshot via `npx playwright@1.49.1 screenshot --browser=webkit`
   (webkit build v2104 is already installed; the global playwright 1.28 CLI crashes on
   this macOS — use the pinned npx version), and assert the terminal row's ink mass is
   within ~3% of the DOM antialiased reference row. Final on-screen sign-off vs real
   Ghostty happens in merge QA.

**Files.** `apps/desktop/src/features/terminal/atlasFontSmoothing.ts` (new),
`TerminalRenderer.ts`, `main.tsx` or the renderer module top for the import.
**Tests.** unit test for the hook (jsdom: detached 2d+willReadFrequently canvas becomes
connected under the holder; other canvases untouched); options assertion for step 2.

---

## 2. IDE chrome should match the terminal scheme (currently near-black)

**Symptom.** Sidebars, tab strip, panels, dialogs are near-black (`#101114`); the
terminal is `#282c34`. The user wants the whole app in the terminal's scheme.

**Root cause.** Chrome colors come from a single token block,
`apps/desktop/src/tokens.css:51-77` (`--chrome-bg: #101114`, `--chrome-raised: #16181c`,
`--chrome-hairline: #1c1e22`, `--chrome-border: #26292f`, `--chrome-hover: #1a1d22`,
`--chrome-selected: #1d2026`, inks `#c9cdd3/#ffffff/#7d848e/#4d545e`). `styles.css`
contains **zero color literals** (only tokens + `rgb(0 0 0 / α)` shadows), so this is a
clean single-point retarget. `#282c34` is the One Dark background, so the new chrome
ramp below is One Dark-adjacent, built as *lighter* deltas off the terminal bg.

**Fix.**

1. Retarget the tokens in `apps/desktop/src/tokens.css`:
   ```css
   --chrome-bg:        #282c34;   /* = --term-bg */
   --chrome-raised:    #2c313a;
   --chrome-hairline:  #313640;
   --chrome-hover:     #2f343e;
   --chrome-selected:  #353b45;
   --chrome-border:    #3e4451;
   --chrome-ink:        #c4c8c6;  /* = --term-7, was #c9cdd3 */
   --chrome-ink-strong: #ffffff;  /* unchanged, = --term-fg */
   --chrome-dim:        #8a919c;  /* lifted from #7d848e for contrast on lighter bg */
   --chrome-faint:      #565e6a;  /* lifted from #4d545e */
   ```
   Note the direction flip: today hairline/hover/selected are *darker* than bg; on
   `#282c34` they must be *lighter* (values above already are). **User decision
   (2026-08-15): stay as close to Ghostty as possible** — so also retarget
   `--accent: #7aa6da` (= `--term-12`) with `--accent-wash` derived from it
   (`#7aa6da1f`), and keep `--danger: #cc6566` (already `--term-1`). `--ok`/`--warn`
   stay (macOS traffic-light colors, not theme colors).
2. Update the two mirrors that must move in lockstep (both are guarded by tests):
   - `apps/desktop/src/features/terminal/theme.ts:81-87` `CHROME_FALLBACKS`
     (asserted equal to tokens.css by `theme.test.ts:95-100`).
   - `apps/desktop/src/features/files/monaco.ts:47-64` — the hardcoded fallback hex in
     every `token("--chrome-*", "#…")` call.
3. Sweep the known seams:
   - `.pane-frame` inset shadow uses `--chrome-bg` as a deliberate black hairline around
     terminals (`styles.css:441`); once chrome == terminal bg it disappears. Decide:
     keep a frame using `--chrome-border`, or drop the shadow.
   - `.state-dot`/`.tab-dot` glyphs are knocked out in `var(--chrome-bg)`
     (`styles.css:291`, `:407`) — still correct after the change (knockout in the new
     bg), just verify visually.
   - Modal backdrop and shadows use `rgb(0 0 0 / α)` literals (`styles.css:516, 745,
     762, 781`) — fine on the lighter scheme, verify visually.
4. Run the visual check across: sidebar, tab strip, right panel (Files + Git), command
   palette, settings dialog, context menus, toasts, disconnect strip.

**Files.** `tokens.css`, `theme.ts`, `monaco.ts`, possibly `styles.css` (pane-frame).
**Tests.** `theme.test.ts` (fallback mirror), `monaco`-related snapshot if any.

---

## 3. Opened files (editor) should match the terminal theme

**Symptom.** Files open on a black editor.

**Root cause.** Monaco uses a custom `"ade-dark"` theme
(`apps/desktop/src/features/files/monaco.ts:40-67`) whose `editor.background` is
`token("--chrome-bg", "#101114")` — the editor is black *solely because chrome is
black*. Syntax colors are inherited vs-dark (`rules: []`), tuned for `#1E1E1E`.

**Fix.**

1. Item 2 fixes the background automatically (editor, gutter, widgets all read chrome
   tokens). Just update the fallback literals (Item 2 step 2).
2. Add syntax `rules` so token colors come from the terminal's ANSI palette instead of
   vs-dark defaults (which sit low-contrast on `#282c34`). In `defineAdeMonacoTheme`:
   ```ts
   rules: [
     { token: "comment",  foreground: "666666", fontStyle: "italic" }, // term-8
     { token: "keyword",  foreground: "b294bb" },                      // term-5
     { token: "string",   foreground: "b6bd68" },                      // term-2
     { token: "number",   foreground: "f0c674" },                      // term-3
     { token: "type",     foreground: "8abeb7" },                      // term-6
     { token: "function", foreground: "82a2be" },                      // term-4
     { token: "variable", foreground: "c4c8c6" },                      // term-7
   ],
   ```
   (Monaco token names are coarse; this is deliberately a small table, not a full
   TextMate map. Verify on a .ts, .rs, .md, .json file.)
3. The two hardcoded diff colors (`monaco.ts` `diffEditor.insertedTextBackground
   #2ea04326` / `removedTextBackground #cc656626`) already fit the palette; keep.
4. Markdown preview is pure chrome-token CSS (`styles.css:723-737`) — follows Item 2
   automatically, no work.

**Files.** `monaco.ts`.
**Tests.** none beyond a visual pass; `AppTabSurface`/`GitDiffSurface` pass
`theme={ADE_MONACO_THEME}` already.

---

## 4. Sidebar: stop showing the workspace working directory

**Symptom.** Each workspace row shows `branch · ~/dev/thing`; with many tabs/terminals
this is noise. Keep the branch, drop the path.

**Root cause.** `WorkspaceSidebar.tsx:172` renders `row.metadata`, which
`workspaceRows.ts:92-96` (`metadataLine`) builds as `` `${branch} · ${path}` `` — branch
and path are **fused into one string** at `workspaceRows.ts:58`. The path also feeds the
row's `aria-label` (`WorkspaceSidebar.tsx:147`) and — important — the ⌘P workspace
switcher's fuzzy-match key (`WorkspaceSwitcher.tsx:30-31`), where matching on the path
is genuinely useful.

**Fix.** Split the field instead of deleting it, so the switcher keeps path search:

1. In `workspaceRows.ts`, replace `metadata?: string` on the row type with two fields:
   `branch?: string` and `path?: string` (keep `sessionPath`/`inferHome`/
   `abbreviateHome` as-is; they now feed `path`). Delete `metadataLine`.
2. `WorkspaceSidebar.tsx:172`: render only `row.branch` in the `.workspace-meta` span.
   Update the `aria-label` join at `:147` to use branch only.
3. `WorkspaceSwitcher.tsx:30-31`: match key becomes
   `` `${row.session.name} ${row.branch ?? ""} ${row.path ?? ""}` `` — search behavior
   preserved.
4. Update `workspaceRows.test.ts` (it asserts `metadataLine` composition) to cover the
   two fields separately.

**Files.** `workspaceRows.ts`, `WorkspaceSidebar.tsx`, `WorkspaceSwitcher.tsx`.
**Tests.** `workspaceRows.test.ts`, `shellComponents.test.tsx` if it snapshots rows.

---

## 5. Sidebar: show up to 3 agents per workspace, then "+N more"

**Symptom.** A workspace row shows at most one agent line; the user wants to see up to
three, with an indicator when more exist.

**Root cause.** By design, `workspaceRows.ts:57` renders a single line from the
*loudest* agent only (`loudestAgentBySession`, `workspaceRows.ts:64-76`, rank
`blocked:4 done:3 working:2 unknown:1 idle:0`). There is no per-row agent list and no
"N more" affordance anywhere in the feature.

**Fix.**

1. In `workspaceRows.ts`, generalize `loudestAgentBySession` to
   `topAgentsBySession(agents, limit = 3)`: same ranking + `updatedAt` tie-break, but
   return the top `limit` agents **and** the total count per session. Keep the exported
   single-agent behavior as `topAgents[0]` so ranks stay in one place.
2. Row type: replace `activity?: string` with
   `agents: { name: string; state: AgentDisplayState }[]` and `agentOverflow: number`
   (total − shown).
3. `WorkspaceSidebar.tsx:171`: render one line per agent (reuse
   `activityWord(displayState(agent))` for the verb), each with its `StateDot` colored
   by state (`--state-*` tokens already exist, `tokens.css:106-109`), then when
   `agentOverflow > 0` a final muted line: `…{agentOverflow} more` (className
   `workspace-activity workspace-activity-more`).
4. CSS: `.workspace-activity` currently clamps to 2 lines
   (`styles.css:244-248`, `-webkit-line-clamp: 2`); change to one flex column of up to
   4 single-line entries (3 agents + overflow), each with `text-overflow: ellipsis`.
5. Accessibility: fold the agent summary into the row `aria-label` as
   `"3 agents, 1 blocked"` style text rather than reading every line.

**Files.** `workspaceRows.ts`, `WorkspaceSidebar.tsx`, `styles.css`.
**Tests.** `workspaceRows.test.ts` (top-3 selection, overflow count, ranking ties),
`agentStatusSurfaces.test.tsx` if it asserts the single-activity line.

---

## 6. Agent panel: rename ordering modes to "status" and "workspace"

**Symptom.** The agents section header toggle cycles `grouped ⇄ priority`; the user
wants `status` and `workspace`.

**Root cause.** `AgentSortMode = "grouped" | "priority"`
(`features/agents/agentsList.ts:14-22`). Investigation found the semantics already match
the requested names: `"priority"` sorts by `compareAgents`, whose ranking *is* the
status ranking (`selectors.ts:10-17`: blocked > done > working > unknown > idle), and
`"grouped"` is a plain sort by workspace order (`byWorkspace`, `agentsList.ts:60+`) —
neither mode renders group headers. **This is a rename, not new sorting logic.**

**Fix.**

1. `agentsList.ts`: `AgentSortMode = "status" | "workspace"`; update `isAgentSortMode`,
   `nextSortMode`, and the `buildAgentRows` mode check (`"status"` → `byPriority`
   comparator, `"workspace"` → `byWorkspace`).
2. **Persisted-state migration** — `features/shell/types.ts:139` currently falls back to
   the default on unknown values, which would silently reset users. Map legacy values
   instead:
   ```ts
   agentSort: shell?.agentSort === "priority" ? "status"
     : shell?.agentSort === "grouped" ? "workspace"
     : isAgentSortMode(shell?.agentSort) ? shell.agentSort
     : defaultShellState.agentSort,
   ```
   Default (`types.ts:98`): `"workspace"` (was `"grouped"` — same semantics).
3. Command palette title hard-codes the names — `commands/registry.ts:138`:
   `"Toggle agent ordering (workspace ⇄ status)"`.
4. The toggle button's visible text is the raw mode string
   (`WorkspaceSidebar.tsx:207-222`) — no change needed beyond the type rename; its
   `aria-label` self-updates.

**Files.** `agentsList.ts`, `types.ts` (shell), `registry.ts`.
**Tests.** `agentsList.test.ts`, `appStatePersistence.test.ts` /
`appStateContract.test.ts` (legacy-value migration), `registry.test.ts` (title).

---

## 7. Settings: saved-host dropdown is too short

**Symptom.** The "Saved host" select in Settings looks squat compared to the text
inputs next to it.

**Root cause.** Two compounding CSS facts
(`apps/desktop/src/features/shell/SettingsDialog.tsx:111-121` has no className on the
select; it's styled only by descendant rules):

1. `styles.css:892-895` gives `.settings-body select` `padding: 6px` but no
   `appearance`/`min-height`; WKWebView's native `menulist` renders at intrinsic
   control height and **ignores most of the padding**, so it sits shorter than the
   sibling inputs that do honor it.
2. `styles.css:891` sets the wrapping label to `font-size: 11.5px`, and the global
   reset `styles.css:33` (`select { font: inherit }`) makes the select inherit that
   caption size, shrinking the native control further.

**Fix.** Style the select as a custom control (matches the app's flat chrome anyway):

```css
.settings-body select {
  appearance: none;
  -webkit-appearance: none;
  min-height: 30px;
  padding: 6px 28px 6px 8px;
  font-size: var(--type-row);            /* 12.5px, matches inputs */
  background-image: url("data:image/svg+xml,…chevron-down, stroke %238a919c…");
  background-repeat: no-repeat;
  background-position: right 8px center;
}
```

Add alongside the existing shared input/select rule at `styles.css:892-895` (keep the
shared rule; this augments it). Check the other dialogs' selects for consistency —
`.download-dialog select` (`styles.css:843-846`) gets deleted anyway by the downloads
item; if any other select survives, consider making this a global `select` style.

**Files.** `styles.css` only.
**Tests.** visual.

---

## 8. Notifications: add a "Test notification" button, and make macOS notifications actually work

**Symptom.** Agent notifications never appear on the user's Mac; there's no way to test
or see permission state.

**Root causes (multiple, ranked).** The pipeline is custom `UNUserNotificationCenter`
code (no tauri-plugin-notification): frontend decision
(`features/agents/notifications.ts:59-115`) → `invoke("emit_agent_notification")`
(`src-tauri/src/lib.rs:75-96`) → `notifications/macos.rs:167-231`.

1. **Foreground suppression is total.** The delegate's `will_present`
   (`macos.rs:86-96`) always completes with `UNNotificationPresentationOptions::empty()`
   — while the app is frontmost, **nothing is ever shown**, yet the frontend only
   suppresses for the exact focused pane (`notifications.ts:81-87`). Any other pane's
   event "succeeds" invisibly. This alone explains "notifications don't work" for a
   user who is looking at the app.
2. **Dev/unsealed builds can't use UNUserNotificationCenter at all.** `pnpm tauri dev`
   runs a bare unbundled binary; a process without a bundle identity/valid seal is
   rejected by the framework. Documented precedent in this repo:
   `tests/acceptance/macos/findings.md` (M10-E016) — the fix (`PlistBuddy` + re-`codesign`)
   lives only in `release/macos/build-package.sh:57-62`. **Notifications are only
   expected to work from that script's output.**
3. **Permission is requested lazily** — only inside `notify()`
   (`ensure_authorized`, `macos.rs:383-412`). If no agent event ever fires, the app
   never appears in System Settings → Notifications and the user can't even grant it.
4. **No sound**: authorization asks for `Alert` only, content never sets a sound
   (`macos.rs:390`, `:186-203`).

**Verified on the user's machine (Phase-0 evidence — cause 2 is live).** The installed
app the user actually runs (`target/release/bundle/macos/tmux Agent IDE.app`, the path
their Claude Code hooks point at) is a plain `tauri build` bundle, **not** the
`release/macos/build-package.sh` output: `Info.plist` still contains
`LSRequiresCarbon = true` (the script deletes it), the signature is adhoc/linker-signed,
and `codesign --verify` fails with "code has no resources but signature indicates they
must be present" — a broken seal. Consistently, `com.apple.ncprefs` has **no entry**
for `dev.muxflow.legacy`: macOS has never registered the app for notifications, so
authorization was never even requestable. Conclusion: on this machine notifications
could never have worked regardless of the code; the packaged-build requirement is not
theoretical. Implementation must still fix causes 1, 3 and 4, and merge-QA must run
against a proper `build-package.sh` bundle.

**Fix.**

*Rust — new surface (all three backends: `macos.rs`, `linux.rs`, `unsupported.rs`):*

1. Add `authorization_status()` to `NativeNotifications`: expose the private
   `notification_status()` (`macos.rs:414-424`) as
   `"authorized" | "denied" | "notDetermined" | "provisional" | "unsupported"`.
   Linux: return `"authorized"` if the notification daemon is reachable, else
   `"unsupported"`. Register as `#[tauri::command] notification_permission_status` in
   `lib.rs` `generate_handler!` (`lib.rs:155-192`).
2. Add `#[tauri::command] emit_test_notification`: calls `ensure_authorized` (this is
   what triggers the OS permission prompt on first use), then posts a notification with
   identifier prefix `"test-"`, title `"tmux Agent IDE"`, body
   `"Test notification — delivery works."`, `request_action: false` (avoids the
   route-storage requirement, `macos.rs:178-185`). Return `Ok`/`Err(String)` so the UI
   can show the exact failure.
3. **Foreground presentation:** in `will_present` (`macos.rs:86-96`), present instead
   of suppressing when the request identifier starts with `"test-"`:
   `completion(Banner | Sound)`. Separately — product decision worth making now —
   loosen the blanket suppression for real agent notifications: suppress only when the
   *app is frontmost AND the notification's pane is the focused pane* (the frontend
   already knows focus; the simplest correct version is to pass a `presentInForeground`
   flag through `emit_agent_notification` computed frontend-side, since the frontend
   already computes focus-based suppression at `notifications.ts:81-87` — then
   `will_present` can honor it via a per-identifier registry, mirroring how
   `route_directory` already keys data by identifier).
4. Request `Alert | Sound` in `ensure_authorized` (`macos.rs:390`) and set the default
   sound on test-notification content.

*Frontend — Settings UI:*

5. In `SettingsDialog.tsx`, Sounds tab (`:170-175`), add a "Notifications" block using
   the existing `settings-host-actions` + `settings-hint` pattern (`:128-143`):
   - Status line from `notification_permission_status` (queried on tab open):
     "Notifications: authorized / denied — enable in System Settings → Notifications /
     not yet requested".
   - **"Send test notification"** button → `emit_test_notification`; on `Err`, render
     the returned message inline (red, `role="alert"`). On success, hint: "If nothing
     appeared, check System Settings → Notifications → tmux Agent IDE."
   - New props (`SettingsDialogProps`, `SettingsDialog.tsx:9-40`):
     `onTestNotification: () => Promise<string | undefined>` (returns error text) or
     similar, wired from `App.tsx:1024-1051`.
6. Add a `docs/troubleshooting.md` subsection: dev builds (`pnpm tauri dev`) cannot
   deliver macOS notifications (M10-E016); test with
   `release/macos/build-package.sh` output.

**Verification.** Build via `release/macos/build-package.sh`; first click of "Send test
notification" must raise the OS permission prompt; grant → banner appears **while the
app is frontmost** (proves the `test-` presentation path); deny → button reports
"denied; enable in System Settings".

**Files.** `notifications/{mod,macos,linux,unsupported}.rs`, `lib.rs`,
`SettingsDialog.tsx`, `App.tsx`, `docs/troubleshooting.md`.
**Tests.** Rust: status mapping + `valid_notification_content` untouched; TS:
`SettingsDialog` interaction test for the new block (mock invoke).

---

## 9. Cmd+T creates a tab but doesn't focus it

**Symptom.** New terminal tab appears; focus/selection stays on the old tab.

**Root cause (three cooperating facts — not a race).**

1. The handler fires and forgets: `useShellCommands.ts:200` —
   `case "window.new": … await options.performAction({ kind: "createWindow", … }); return;`
   The result is discarded, though `TmuxActionResult` **already carries `windowId`**
   (`features/tmux/actions.ts:30-35`; populated by the host at
   `apps/host/src/service/tmux_actions.rs:103-107`).
2. The host creates the window **detached**: `new-window -d`
   (`apps/host/src/service/tmux_actions/command.rs:9-24`), so tmux's active window
   doesn't change.
3. The app mirrors tmux's active flag on every snapshot:
   `resolveActiveWindowId` prefers `windows.find(w => w.active)`
   (`app/windowSelection.ts:25-32`, applied by the effect at
   `useAppConnectionController.ts:143-145`) — so even an optimistic local
   `setActiveWindowId(newId)` would be clobbered by the next snapshot.

**Fix (frontend, mirroring the existing `session.new` pattern at
`useShellCommands.ts:172-182`).**

1. Add an option callback `selectCreatedWindow(sessionId, windowId)` to
   `useShellCommands` options; `App.tsx` (near `selectCreatedSession`, `App.tsx:569`)
   implements it as: `performAction({ kind: "selectWindow", sessionId, windowId })`,
   then on acceptance `setActiveWindowId(windowId)`. Do **not** route through
   `requestActiveWindow` (`windowSelection.ts:34-50`) — it requires the window to
   already exist in the snapshot's `windows` list, which lags creation.
   `SelectWindow` runs a real `tmux select-window` with an `item.active` postcondition
   (`tmux_actions.rs:125-129`, `:298-301`), so subsequent snapshots keep the new tab
   active and fact 3 works *for* us instead of against us.
2. Rewrite the case:
   ```ts
   case "window.new": {
     if (!targetSession) return;
     const scope = options.currentScope();
     void options.performAction({ kind: "createWindow", sessionId: targetSession.id })
       .then((result) => {
         if (result?.windowId && options.isHostScopeCurrent(scope))
           options.selectCreatedWindow(targetSession.id, result.windowId);
       });
     return;
   }
   ```
   (Scope guard identical to `session.new` — don't steal focus if the user switched
   hosts mid-flight.)
3. Leave `new-window -d` alone in the host — the `-d` is shared with the split path
   (`command.rs:38`) and the postcondition at `tmux_actions.rs:283-286` doesn't assert
   activity; changing host semantics is higher blast radius for zero extra benefit.

**Files.** `useShellCommands.ts`, `App.tsx`.
**Tests.** `useShellCommands`-covering tests (`registry.test.ts` isn't it — look at
`App.integration.test.tsx`): new tab command selects the created window; scope-changed
case does not.

---

## 10. Cmd+L should toggle the file-explorer (right) panel

**Symptom.** Cmd+B toggles the left sidebar (works); Cmd+L does nothing.

**Root cause.** The right-panel toggle exists end-to-end
(`view.togglePanel` → `shellAfterSidebarCommand` → `panelOpen`,
`responsiveShell.ts:39-53`, `App.tsx:951`) but is bound to **⌥⌘B**
(`commands/registry.ts:127`). `Meta+L` is bound to nothing — zero grep hits, and
`commandForKeyboardEvent` returns `undefined` for unbound keys, so the keydown falls
through without `preventDefault`. Bonus context: `registry.ts:280-311` documents that
Option-modified letters historically broke on macOS (⌥B → `∫`), which is a good reason
to move this off Option entirely.

**Fix.** One line — `registry.ts:127`:

```ts
{ id: "view.togglePanel", title: "Toggle right panel", group: "View",
  defaults: { mac: "Meta+L", linux: "Ctrl+L" } },
```

Confirm `shortcutCollisions()` (`registry.ts:351-362`) stays empty (a duplicate would
silently disable **both** commands due to the exactly-one-match rule). Note: while a
terminal pane is focused, Cmd+L now toggles the panel instead of reaching the shell —
same tradeoff Cmd+B already makes; consistent.

**Files.** `registry.ts`. **Tests.** `registry.test.ts` (defaults + no collisions).

---

## 11. File explorer: VS Code-like icons

**Symptom.** Rows show text glyphs — literally `d` (directory), `l` (symlink), `·`
(file) — plus a chevron. Hard to scan.

**Root cause.** There is no icon system for files. `ui/Icon.tsx` is a hand-drawn inline
SVG set of 14 general icons (16×16, `stroke="currentColor"`, strokeWidth 1.4); the
explorer row's "icon" slot is a `.file-state` span with the letter glyph
(`ExplorerTree.tsx:214-218`, styled `styles.css:577-579`). No folder icon exists; the
only file-type mapping in the codebase is `DOCUMENT_ICON` in `TabStrip.tsx:22-26`
(3 entries, keyed by tab kind).

**Fix.** Stay with the codebase's no-dependency inline-SVG approach; add shape
archetypes colored per extension (VS Code's Seti feel comes ~80% from color):

1. `ui/Icon.tsx`: add icon names + paths (16×16 stroke style, consistent with the
   existing set): `folder`, `folderOpen`, `fileCode` (angle brackets), `fileText`,
   `fileConfig` (gear), `fileImage`, `fileLock`, `fileShell` (prompt chevron),
   `fileData` (braces). (`markdown`, `file`, `diff`, `branch` already exist.)
2. New module `features/files/fileIcons.ts`:
   ```ts
   export function fileIcon(entry: { name: string; kind: FileEntry["kind"] }):
     { icon: IconName; color?: string }
   ```
   - directories → `folder`/`folderOpen` (expanded passed by caller), color
     `var(--term-4)` (matches today's directory tint);
   - extension table (color from the ANSI palette): `.ts/.tsx` → fileCode `--term-4`;
     `.js/.jsx` → fileCode `--term-3`; `.rs` → fileCode `--term-1`; `.py` → fileCode
     `--term-2`; `.json/.toml/.yml/.yaml` → fileData `--term-3`; `.md` → markdown
     `--term-4`; `.css/.scss` → fileCode `--term-5`; `.html` → fileCode `--term-1`;
     `.sh/.zsh/.bash` → fileShell `--term-2`; `.png/.jpg/.svg/...` → fileImage
     `--term-5`; `.lock` → fileLock `--chrome-faint`; dotfiles/config
     (`.gitignore`, `.env*`, `*.config.*`) → fileConfig `--chrome-dim`; fallback →
     `file`, `--chrome-dim`. Symlinks: keep target-kind icon + retain the `l` tint
     via color `--term-6` (or add a small link badge later).
3. `ExplorerTree.tsx:214-218`: replace the `.file-state` letter span with
   `<Icon name={…} size={14} />` wrapped in a span carrying `style={{ color }}`.
   Widen the grid's icon column: `styles.css:570`
   `grid-template-columns: 12px 16px minmax(90px, 1fr)`.
4. Reuse `fileIcon` in `TabStrip.tsx` (`DOCUMENT_ICON`) so editor tabs get the same
   per-type icon — one mapping, two surfaces.

**Files.** `Icon.tsx`, new `fileIcons.ts`, `ExplorerTree.tsx`, `styles.css`,
`TabStrip.tsx`.
**Tests.** new `fileIcons.test.ts` (extension → icon/color table, dotfile handling);
`ExplorerTree.test.tsx` render assertions (`d`/`·` glyphs are asserted there today —
update).

---

## 12. File explorer: font

**Symptom.** Tree text "looks weird" — it's 11px **monospace**.

**Root cause.** `.file-main` sets `font-family: var(--font-mono); font-size: 11px`
(`styles.css:570-576`). VS Code's explorer is the UI sans font at 13px on 22px rows.

**Fix.** In `.file-main` (and `.explorer-root`, `styles.css:549-561`):
`font-family: var(--font-sans); font-size: var(--type-row);` (12.5px — the token exists
and is unused here today). Keep `height: 22px` rows (VS Code's exact row height).
Check nothing depended on monospace alignment (the old letter glyphs did; they're gone
after Item 11).

**Files.** `styles.css`. **Tests.** visual.

---

## 13. File explorer: hide `.git` (and friends), VS Code-style

**Symptom.** `.git` shows as a row. User wants it (and similar noise) gone, like
VS Code's `files.exclude` defaults.

**Root cause.** The host daemon produces listings with **no entry filtering** —
`.git`/`node_modules` are only made non-expandable ("collapsed"):
`apps/host/src/service/filesystem.rs:240,255` and the descend/watch guard at
`service/filesystem/listing.rs:66-70,143-147`. An existing test *asserts* `.git`
appears (`filesystem.rs:407-434`,
`listing_keeps_dotfiles_and_collapses_heavy_and_symlink_directories`).

**Fix (host-side, so every client benefits and pagination stays consistent).**

1. In the listing loop (`listing.rs:79-97`), skip always-hidden names before insertion.
   VS Code's `files.exclude` defaults, adopted here:
   `.git`, `.svn`, `.hg`, `CVS`, `.DS_Store`, `Thumbs.db`. Define once:
   ```rust
   const ALWAYS_HIDDEN: &[&str] = &[".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db"];
   ```
   Keep `node_modules` **visible but collapsed** — that matches VS Code (it does not
   hide node_modules) and today's behavior.
2. Keep the `.git` component in the descend-guard (`listing.rs:66-70`) — hidden
   entries must also stay unenterable if a path is requested directly.
3. Both `collapsed` computations (`filesystem.rs:175` and `:240` — duplicated logic)
   should reference a shared helper alongside `ALWAYS_HIDDEN` so the two rules can't
   drift.
4. Update the test: `.git` no longer present; `node_modules` still present and
   collapsed; `.DS_Store` filtered; `.env` still visible (dotfiles in general stay).

**Files.** `apps/host/src/service/filesystem.rs`, `filesystem/listing.rs`.
**Tests.** the named host test; run `pnpm test:shell` / host test lane
(`cargo test -p tmux-ide-host` or whatever `tests/phase*` lane covers filesystem).

Note on sort order: current order is dirs-first then raw byte order (dotfiles first,
`Z` before `a`) — user said dotfiles-on-top is fine, and the BTreeMap key doubles as
the pagination token (`listing.rs:151-177`), so **do not** change collation in this
round; note case-insensitive sort as a possible follow-up with a token-format bump.

---

## 14. File explorer: hide git-ignored files

**Symptom.** Ignored build artifacts (e.g. `target/`, `dist/`) clutter the tree.

**Root cause / existing leverage.** The explorer has no ignore-awareness, but the git
lane already computes it end-to-end: the host runs
`git status --porcelain=v2 … --ignored=matching`
(`apps/host/src/service/git/status.rs:153-170`), the parser marks entries
`ignored: true` and collapses ignored directories to a single path
(`git/parser.rs:74-102`), the proto carries it (`envelope.proto:554`), and the frontend
already receives `entry.ignored` (`features/git/api.ts:21,215`, `git/types.ts:29`) —
`GitSidebar.tsx:258` already filters on it. `App.tsx` has both `workspaceFiles` and
`workspaceGit` in scope (`App.tsx:238`).

**Fix (frontend filter — no proto/host changes).**

1. Build the ignore set where both controllers meet (App.tsx or a small hook):
   ```ts
   const ignored = useMemo(() => {
     const status = workspaceGit.status;
     if (!status?.repository.authoritative) return undefined;   // degrade: show all
     const root = status.repository.worktreeRoot;
     return new Set(status.entries.filter(e => e.ignored)
       .map(e => `${root}/${e.displayPath}`));
   }, [workspaceGit.status]);
   ```
   (Use `displayPath` — repo-relative; never decode the opaque `path` field.)
2. Pass `ignoredPaths` into `ExplorerTree` and filter in `flattenTree`
   (`ExplorerTree.tsx:289-301`): drop an entry when its absolute path is in the set
   **or has a prefix in the set** (git reports an ignored directory once, not its
   contents — a simple `startsWith(prefix + "/")` check against the set's directory
   entries; precompute the directory prefixes).
3. Degradation rules (all "show everything"): no git worktree
   (`useWorkspaceGit.ts:30` never fetches), status non-authoritative/oversized
   (`git/status.rs:174-190` returns zero entries with `authoritative: false`).
4. Add a small escape hatch now, cheaply: context-menu toggle "Show ignored files" on
   the explorer header (local component state, default hidden). VS Code parity.

**Files.** `App.tsx`, `ExplorerTree.tsx`, maybe `useWorkspaceFiles.ts`.
**Tests.** `ExplorerTree.test.tsx` (filtering, prefix case, degradation when
`authoritative: false`).

---

## 15. File explorer: single-click = preview tab, double-click = pin (VS Code)

**Symptom.** Every single click opens another permanent tab; N clicks = N tabs.

**Root cause.** `openFileTab` (`features/shell/model.ts:209-239`) dedupes by resource
but always appends a permanent tab otherwise; there is **no preview/pinned concept**
anywhere. Explorer single-click routes straight to it
(`ExplorerTree.tsx:209,214` → `App.tsx:663-675` `openExplorerEntry`). Double-click is
currently unused on app tabs (`TabStrip.tsx:105` uses it only for terminal-tab rename).

**Fix.**

1. **Data model** — add `preview?: boolean` to `AppOwnedTab`
   (`features/shell/types.ts:17-37`). The persistence contract requires touching, in
   lockstep (this exact mismatch once silently killed persistence — see the `_comment`
   in the contract file):
   - `features/shell/persistedAppState.contract.json` — add the field to the fixture;
   - Rust mirror `AppTabRecord`, `src-tauri/src/app_state.rs:18-47` — add
     `#[serde(default)] preview: Option<bool>` (old saves lack it);
   - the key-set assertion at `app_state.rs:652-666` ("eighteen fields" → nineteen);
   - `features/shell/appStateContract.test.ts`.
2. **Open logic** — `openFileTab(state, …, { preview: boolean })`:
   - existing tab for this resource → select it; if it was preview and this open is a
     pin (double-click), clear `preview`;
   - no existing tab and `preview: true` → if the workspace already has a preview tab,
     **replace it in place** (same `id`? No — reuse the slot: same `order`, new `id`,
     new resource/title/kind — simplest is to mutate the existing preview tab's
     resource/title/kind/rootPath and reset viewMode) and select it;
   - no preview tab → append one with `preview: true`;
   - `preview: false` → today's behavior.
   Pin-on-edit: when the editor buffer becomes dirty, clear `preview` — hook where
   autosave/dirty state is tracked (`features/files/autosave.ts` /
   `editorFlushRegistry.ts`); a `pinAppTab(state, tabId)` helper in `model.ts` keeps
   it one-line at the call site.
3. **Explorer wiring** — `ExplorerTree.tsx`: single-click (`:209`, `:214`) calls
   `props.onOpen(entry, { preview: true })`; add `onDoubleClick` on `.file-main`
   calling `props.onOpen(entry, { preview: false })` (click fires first and creates
   the preview; the double-click then pins the same tab — matches VS Code). Keyboard
   Enter (`:88-91`) → pin (VS Code behavior).
4. **Tab strip** — `TabStrip.tsx:117`: render preview titles in italic
   (`className={tab.preview ? "tab-title tab-title-preview" : "tab-title"}`;
   CSS `font-style: italic` — italic JetBrains Mono is already bundled,
   `tokens.css:36-42`, and tabs use the sans stack anyway so italic just works).
   Add `onDoubleClick` on app tabs → pin (`TabStrip.tsx:105` free for app tabs).
5. `CombinedTab` (`model.ts:7-9`) needs `preview` exposed for the strip.

**Files.** `types.ts`, `model.ts`, `App.tsx`, `ExplorerTree.tsx`, `TabStrip.tsx`,
`contract.json`, `app_state.rs`, `styles.css`.
**Tests.** `model.test.ts` (replace-in-place, pin transitions, one-preview-per-
workspace invariant), `appStateContract.test.ts`, Rust `app_state.rs` tests,
`ExplorerTree.test.tsx` (single vs double click callbacks).

---

## 16. Downloads: OS file picker immediately, auto non-colliding name, completion actions

**Symptoms.** (a) Download opens an in-app modal where you must click "Choose…" before
the OS save panel appears, then "Start download". (b) A collision-policy dropdown asks
you to pre-choose stop/rename/overwrite. (c) "Download complete" toast has no
Open / Open-folder actions — and it shows the *remote* path.

**Root causes.**

- The modal (`features/files/DownloadDialog.tsx`) exists only to collect a destination
  string + policy; the OS panel is behind its "Choose…" button (`:28-34`,
  `@tauri-apps/plugin-dialog` `save()`, permitted via `dialog:allow-save`).
- The collision `<select>` (`:36-41`, default `"fail"`) is shown unconditionally;
  enforcement is backend `choose_name()`
  (`src-tauri/src/connection/files/local_destination.rs:718-751`) with a `rename`
  policy that already generates `name (1).ext` (`:762-793`), and the backend already
  echoes the **final** published path (`download_manager.rs:428` →
  `features/files/api.ts:404`).
- The completion toast pipeline (`downloadStatus.ts:10-20`) formats the **remote
  source** path (`active.path`) and the toast markup only supports Dismiss
  (`App.tsx:1003-1012`) — though sibling toasts already have action buttons
  (`App.tsx:1014-1020`), so the pattern exists.
- No opener exists: `external_links.rs` rejects non-http(s) and shells `xdg-open`
  unconditionally (Linux-only); no opener/shell plugin, `NSWorkspace` linked but
  unused for this (`Cargo.toml:39`).

**Fix.**

*Flow (frontend):*

1. Delete `DownloadDialog.tsx` + its render in `AppDialogLayer.tsx:101` and the
   `pendingDownload` state (`App.tsx:154`). The two entry points (`App.tsx:960`,
   `:922`) call a new `startDownloadFlow(request, root)`:
   ```ts
   const selected = await save({
     title: request.kind === "folder" ? "Save folder archive" : "Save file",
     defaultPath: suggestedDownloadName(request),   // keep this helper
   });
   if (!selected) return;                            // user cancelled — done
   void startDownload({ ...request, destination: selected, collision: "overwrite" }, root);
   ```
2. Collision policy: pass **`overwrite`** (wire `overwriteConfirmed`, mapping already
   at `api.ts:204`). Rationale: the macOS save panel itself asks "…already exists.
   Replace?" whenever the user picks an existing name — that native confirmation *is*
   the consent, and silently renaming after the user clicked "Replace" would be wrong.
   The user's "auto non-colliding name" ask is satisfied *before* the panel opens
   instead (step 3), so the replace prompt effectively never appears unless the user
   deliberately targets an existing file.
3. Auto non-colliding **default**: add a tiny Tauri command
   `suggest_download_destination(file_name: String) -> String` that resolves the
   user's Downloads dir and applies the existing `renamed_name_bytes` logic
   (`local_destination.rs:762-793`, refactor it so both call sites share it) until the
   name is free — returning e.g. `~/Downloads/report (2).pdf`. Use it as
   `defaultPath`. Result: open picker → name already unique → click Save → done.
   (Keep `fail` semantics out of it entirely; the backend still protects against
   TOCTOU via `choose_name`'s overwrite-must-be-regular-file rule.)
4. Remove the now-dead `CollisionPolicy` UI plumbing; keep the type (wire still uses
   it) and `DownloadDialog.test.tsx` gets replaced by a test of `startDownloadFlow`
   (picker cancelled / confirmed paths).

*Completion toast:*

5. `downloadStatus.ts:19`: use the transfer's **local destination**
   (`transfer.destination`, `features/files/types.ts:113` — populated from the
   backend's final-path echo, so a renamed file shows its real name):
   `` `Download complete: ${transfer.destination}` `` (and keep state wording for
   failed/cancelled).
6. Toast actions — extend the notice rendered at `App.tsx:1003-1012` following the
   `App.tsx:1014-1020` action-button pattern: when the notice is a completed download,
   add **Open** and **Show in Finder** buttons.

*Opener (Rust):*

7. New commands in `src-tauri` (register in `lib.rs` `generate_handler!`):
   - `reveal_download(path)` — macOS: `NSWorkspace activateFileViewerSelectingURLs`
     (objc2-app-kit already links NSWorkspace); Linux: `dbus`-free fallback
     `xdg-open <parent dir>`.
   - `open_download(path)` — macOS: `NSWorkspace openURL` (file URL); Linux:
     `xdg-open <path>`.
   Security: do **not** accept arbitrary paths from the renderer. Keep a
   `Mutex<HashSet<PathBuf>>` of completed download destinations in the download
   manager's state (insert where the final path is published,
   `download_manager.rs:428`); both commands refuse paths not in the set. This keeps
   the renderer→OS-open surface limited to files the app itself just wrote.
8. Also: the ExplorerTree downloads list (`ExplorerTree.tsx:259-274`) gets the same
   two actions on completed rows — same commands, second surface, cheap.

**Files.** `DownloadDialog.tsx` (delete), `AppDialogLayer.tsx`, `App.tsx`,
`downloadStatus.ts`, `api.ts`, `ExplorerTree.tsx`,
`src-tauri/src/connection/files/{download_manager,local_destination}.rs`, `lib.rs`.
**Tests.** `downloadStatus.test.ts` (destination in message),
`local_destination` Rust tests (shared rename helper), new flow test; manual: download
same file 3× → `x.pdf`, `x (1).pdf`, `x (2).pdf` with zero prompts.

---

## 17. Claude Code hook errors outside tmux (`TMUX_PANE is unavailable`)

**Symptom.** Running Claude Code in a plain terminal (not inside tmux) shows on every
prompt: `UserPromptSubmit hook error … Error: TMUX_PANE is unavailable`.

**Root cause.** The managed hook installed into `~/.claude/settings.json` (generated by
`hook_command()`, `apps/host/src/service/agents/adapters.rs:43-51`) runs
`tmux-ide-host hook ingest …` on every hook event, unconditionally. `hook ingest`
treats a missing pane as a hard error: `apps/host/src/hook.rs:34`
```rust
let pane_id = std::env::var("TMUX_PANE").context("TMUX_PANE is unavailable")?;
```
A Claude Code session outside tmux has no pane — there is legitimately nothing to
ingest, but the non-zero exit surfaces as a hook error in every such session.

**Fix.**

1. In `hook.rs run()`, make the no-tmux case a silent no-op (after reading stdin —
   stdin must still be drained so the hook protocol isn't left with a blocked pipe;
   the read already happens first at `:21-24`):
   ```rust
   let Ok(pane_id) = std::env::var("TMUX_PANE") else {
       return Ok(());   // not inside tmux → this session is not attached to any IDE pane
   };
   ```
   Keep `validate_pane_id` strict for *present-but-malformed* values — that still
   indicates a real misconfiguration and should fail loudly.
2. Exit code 0 with no output means Claude Code (and Codex — same adapter mechanism)
   shows nothing. No `--managed-version` bump needed: the installed command line is
   unchanged; only the binary behavior changes.
3. Add a test beside the existing hook tests in `hook.rs`: unset `TMUX_PANE`, feed a
   valid JSON object, assert `Ok` and that no fallback mailbox file is written.

**Files.** `apps/host/src/hook.rs`.
**Tests.** hook unit test; sanity: run `claude` outside tmux → no hook error line.

---

## 18. Terminal pane renders narrower/smaller than the pane (letterboxing; quarter-screen sessions)

**Symptoms.** Two presentations of the same defect class, both captured live:
(a) On the remote host, a workspace's terminal content fills only part of the pane —
Claude Code's full-width UI (input box background, separators) stops mid-pane and the
slack is plain `--term-bg`, which reads as "the background color is weird and the pane
is too narrow". (b) A freshly used workspace (`notif-test`) renders at literally a
quarter of the surface.

**Root cause — verified against the live remote tmux server (remote-linux).** Snapshot taken
while the app was showing `notif-test`:

```
client-2213143  size=159x  flags=control-mode,pause-after=5              session=inductive   ← the ONLY sizing participant
client-2213147  size=80x   flags=control-mode,ignore-size,pause-after=5  session=ks
client-2213155  size=80x   flags=control-mode,ignore-size,pause-after=5  session=notif-test  ← visible, still ignore-size
/dev/pts/42     size=239x57  (plain ssh client)                          session=inductive
/dev/pts/26,32  size=188x51  (plain ssh clients)                         sessions existing-terminal-tool, inductive
window-size latest
notif-test windows: 80x24 ×3        ← tmux's default size: never told anything else → quarter screen
existing-terminal-tool windows:      188x50           ← sized by the pts clients, not by the app → letterbox
```

The desktop's sizing architecture: one control client per session; all attach with
`ignore-size`, and only the *visible* session's client is taken out of it and sent
`refresh-client -C` (`apps/host/src/service/terminal.rs:220-229`, `:274-284`
`set_sizing`, and the `TerminalClients.last_size` handoff at `:307-324`, which exists
precisely because of finding M13-E005). Two distinct defects:

- **Defect A — RESOLVED by Phase-0 code trace: the visible-session signal does not
  exist after connect.** The host's `select_session` (the correct "one act" flag+size
  mechanism, `terminal.rs:391-420,453-466`) has exactly **two** callers:
  `Operation::AttachTerminal` — which the desktop bridge sends **once, at connect**,
  for the then-selected session (`src-tauri/src/connection/bridge.rs:208-225`) — and
  the post-`createSession` hook (`service/requests/tmux_action_dispatch.rs:144`). The
  message that *is* sent on workspace switches, `SetTerminalVisibility`, is pane-level
  reveal/hide and never touches sizing (`terminal.rs:476+`,
  `dispatcher.rs:355`). Consequence: the sizing participant never moves after connect;
  worse, every later `refresh-client -C` from `TerminalSessions::resize`
  (`terminal.rs:437-451`) is addressed to the **stale** `visible_session`. This exactly
  reproduces the live observation (`inductive`'s client participating at 159 cols
  while `notif-test` was displayed with `ignore-size` at 80x24).
- **Defect B — the app never re-asserts its size.** With `window-size latest`, any
  activity in the plain SSH clients resizes the shared windows to *their* size
  (`existing-terminal-tool` at 188x50). The app then letterboxes — and stays letterboxed, because
  `useClientResize` dedupes against **its own last request**
  (`apps/desktop/src/app/useClientResize.ts:96`) and nothing anywhere compares the
  authoritative snapshot's actual pane cell size (`Pane.width/height` is already on
  the wire — `crates/protocol/proto/envelope.proto:228-229`, frontend
  `apps/desktop/src/app/types.ts:26`) against what was requested. Input sent through
  the control client doesn't make it the "latest" client, so even active app use
  never wins the size back.

**Fix.**

1. **Restore the visible-session signal (Defect A, primary fix):** on active-session
   change, the desktop must tell the host. Two options — (a) re-send
   `Operation::AttachTerminal` for the newly selected session (the host's `attach()`
   already handles the already-attached case: `select_session` + reseed,
   `terminal.rs:352-366`; note this reseeds the listed panes — acceptable, a workspace
   switch reseeds anyway), or (b) add a lighter `SelectSession` operation that calls
   only `select_session`. Prefer (a) unless the reseed cost proves noticeable — it
   needs no proto change. Wire it from wherever the desktop's active session state
   changes (a new Tauri command on the connection, called from the workspace-switch
   path in `App.tsx`/`useAppConnectionController.ts`).
2. **Desktop reconciliation (Defect B, and belt-and-braces for A):** feed the active
   window's *actual* cell size (from the snapshot's panes for that window) into
   `useClientResize`. When the app's OS window has focus and actual ≠ `lastRequested`,
   clear `lastRequested.current` and schedule `send()` (existing debounce). This turns
   the dedupe from "what I last asked" into "what tmux actually has".
3. **Reassert on focus gain:** listen to the Tauri window focus event and trigger the
   same recompute — under `window-size latest` semantics, the user returning to the app
   is exactly when the app should become the sizing client.
4. **Never fight while unfocused:** no reassertion when the app window is not focused —
   the user is legitimately working in the other terminal; resizing under them is the
   P12-U006 class of harm. This asymmetry (reassert on focus, yield while unfocused) is
   the whole anti-resize-war policy.
5. **Host logging:** log the visibility handoff (`set_sizing` + follow-up `resize`)
   with client name and result in the daemon log, so any future silent
   `refresh-client -f` failure is diagnosable. Extend the M13-E005 regression test
   with a workspace-switch sequence exercising the new signal from step 1.
6. Verify on the remote host (merge QA): switch to a fresh workspace → windows leave
   80x24 within one debounce; shrink via a small SSH client, type there, refocus the
   app → app re-wins; Claude Code's input box reaches the pane edge.

**Files.** `useClientResize.ts`, `clientSize.ts` (or a sibling selector for
actual-size), `App.tsx`/`useAppConnectionController.ts` (plumb pane sizes + focus),
`apps/host/src/service/terminal.rs` (logging, handoff regression test).
**Tests.** `useClientResize.test.tsx` (external-shrink → resend only when focused),
host terminal tests; manual matrix above. Note: after Item 2 the letterbox slack color
matches the chrome so the *seam* softens, but the real fix is this reassertion.

---

## 19. Toast noise: only notify what has no visible effect

**Symptom.** Toasts like "Closed test-runs-docs-raw.json" appear for actions
whose result is already visible on screen (the tab closed). The user wants toasts only
for events with **no** other indication — download complete (with Open / Show in
Finder, Item 16), upload complete (an image paste *is* an upload, so that one may
stay), errors.

**Root cause.** `setStatus` is used both for errors and for routine success
confirmations, and every status becomes a toast via `noticeForStatus`
(`features/shell/statusNotice.ts:73-80`, rendered at `App.tsx:1003-1012`). The noisy
call sites found:

- `useShellCommands.ts:105` — `Closed ${targetAppTab.title}` (file-tab close; visible).
- `App.tsx:542` — `Opened ${tab.title}` (agent navigation opens the tab on screen).
- `App.tsx:699` — `File ${mutation.kind} completed.` (rename/create/delete — the tree
  updates visibly).

**Fix.**

1. Delete those three `setStatus` calls (keep the *error* branches next to them —
   e.g. `App.tsx:701`, `useShellCommands.ts:101` stay).
2. Keep: connection state, gap/resync, destructive-outcome errors, download/upload
   completion (download completion gains action buttons in Item 16 — that's the class
   of toast the user wants).
3. While there, audit the remaining `setStatus` call sites in `App.tsx` /
   `useShellCommands.ts` against the rule: **toast only if the user cannot see the
   effect or must act**. Anything else is log material, not a toast.

**Files.** `useShellCommands.ts`, `App.tsx`.
**Tests.** whichever `shellComponents`/integration tests assert those statuses.

---

## 20. Don't confirm closing a terminal tab

**Symptom.** Closing a terminal tab raises a "Close current tab — … Running processes
in it will be terminated" dialog. The user doesn't want it (their Ghostty runs
`confirm-close-surface = false` — this is a deliberate preference, not an oversight).

**Root cause.** `closeWindow`/`closePane`/`closeSession` are all classified destructive
(`features/tmux/actions.ts:98-100`), and destructive commands route through
`createTmuxConfirmation` → modal (`useShellCommands.ts:108-140`,
`commands/destructiveConfirmation.ts:14-30`), which stamps `confirmed: true` on the
action only after the dialog.

**Fix.**

1. In `useShellCommands.ts`, for `closeWindow` and `closePane`: skip
   `setConfirmation` and dispatch immediately with the same payload the dialog's
   accept path produces — `{ ...action, confirmed: true }` plus the authoritative
   precondition (`{ serverIdentity, generation }`). The host contract
   (`docs/destructive-actions.md`) is unchanged: the `confirmed` flag still arrives;
   only the UI gate is removed.
2. Keep the dialog for `closeSession` (closing a whole workspace with all its windows
   is a different blast radius, and the user's complaint was tab close).
3. Leave `isDestructiveTmuxAction` itself alone — it also gates precondition checking,
   not just the dialog.

**Files.** `useShellCommands.ts`.
**Tests.** `destructiveConfirmation.test.ts` / command tests: closeWindow no longer
sets a pending confirmation; closeSession still does.

---

## 21. Pane output freezes until you switch tabs and back

**Symptom.** A terminal pane stops updating (e.g. Claude Code's trust prompt frozen
after a keypress). Clicking in/out doesn't help; switching to another tab and back
refreshes it.

**Root cause.** The control clients attach with `pause-after=5` (visible in the live
client flags above): tmux pauses a pane's output stream when the client can't keep up.
The host handles `%pause` by emitting `TerminalFlowPaused` and issuing **one**
capture-with-resume (`apps/host/src/service/terminal/stream.rs:420-435` →
`refresh-client -A '%N:continue'`, `terminal.rs:647-692`). The code itself documents
the failure mode this leaves behind:

- `stream.rs:152` — "A rejected resume leaves that pane paused forever".
- `stream.rs:367` — logs "tmux rejected the flow-control resume for {pane_id}; the
  pane stays paused until it is reseeded".
- Output produced while paused is **dropped, never replayed** (`stream.rs:484-488`),
  so the recovery capture is the only thing that can bring the pane back.

"Reseeded" is exactly what a tab switch does (pane reveal → seed request) — which is
the user's observed workaround, verbatim. So: a pane got paused, its single resume was
rejected or lost, and nothing retries.

**Phase-0 confirmation:** a repo-wide grep shows the desktop frontend has **zero**
handling of `TerminalFlowPaused` — the event is emitted by the host and dropped on the
floor client-side. Recovery today rests entirely on the host's single unretried
resume, exactly as hypothesized. (The precise trigger of the user's specific stall is
still to be reproduced via the stall lane — that remains step 1.)

**Fix.**

1. **Reproduce first** with the existing stall lane (`pnpm test:performance:stall`,
   `tests/performance/runtime/run-stall.sh`); add a fixture that forces a rejected resume so the
   fix is testable, not speculative.
2. **Host — retry with escalation:** on the rejected-resume path (`stream.rs:367`),
   retry the resume once after a short delay; if it fails again, emit a dedicated
   event (e.g. `TerminalFlowStalled` with the pane scope) instead of only writing a
   daemon log line nobody sees.
3. **Desktop — self-heal:** on `TerminalFlowPaused`/stalled events for a *visible*
   pane (check the event switch in `useAppConnectionController.ts` — `TerminalFlowPaused`
   may currently be unhandled), trigger the same reseed used on pane reveal
   (`PaneRecovery` / `request_seed`, `terminal.rs:266-272`) automatically instead of
   waiting for the user to switch tabs.
4. Consider whether `pause-after=5` is the right constant for high-throughput panes,
   but only after 1–3: raising it hides the bug, it doesn't fix the missing recovery.

**Files.** `apps/host/src/service/terminal/stream.rs`, `terminal.rs`,
`useAppConnectionController.ts`, `features/terminal/PaneRecovery.ts`.
**Tests.** phase12 stall lane fixture; stream.rs unit tests around the resume
bookkeeping (`expected_resume`, `stream.rs:477`).

---

## Verification checklist (whole round)

- `pnpm check && pnpm test` (desktop), `cargo test` (workspace) green.
- Host lanes touched: `pnpm test:shell` (filesystem listing), `pnpm test:protocol`
  if hook plumbing is covered there.
- Fresh macOS package via `release/macos/build-package.sh` for the notification work —
  dev builds cannot deliver notifications (M10-E016).
- Persistence round-trip after Items 6/15: launch with an old persisted state file
  (legacy `agentSort` values, tabs without `preview`) and confirm nothing resets.
- Side-by-side Ghostty vs app screenshot at 400% zoom for Item 1.
- Remote-host session (Items 18/21): fresh workspace leaves 80x24 immediately; app
  re-wins sizing on focus; `pnpm test:performance:stall` lane green with the new fixture.
