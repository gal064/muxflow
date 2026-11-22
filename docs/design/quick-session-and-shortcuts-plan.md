# Xcode 27, dependencies, keyboard settings and quick sessions

Status: in progress on `feat/quick-session-shortcuts-xcode27`. Base: main at
50fc1f20 (21 Nov 2026).

Mocks: the Design canvas "Muxflow shortcuts mocks" (desktop Settings ›
Keyboard, hold-⌘ badges after ⌘⇧N, mobile New agent | New terminal bar,
the screen after a tap, mobile Settings).

## 0. Summary and the owner's decisions

| # | What | Decision |
|---|---|---|
| 1 | App Store warning ITMS-90725: build 9 used the iOS 26.5 SDK | Build with Xcode 27 |
| 2 | Dependencies | Update all of them. Xcode 27 and Expo 58 go in one PR, with thorough manual QA |
| 3 | Shortcut editor | Moves into Settings as a Keyboard tab |
| 4 | ⌘1–9 | A setting: jump to Workspaces (today) or Agents |
| 5 | Numbers on agent rows | Shown while ⌘ is held. Always on, not a setting |
| 6 | ⌘⇧N | New agent session in the workspace named `random`, created if missing |
| 7 | Default agent | A per-host "Agent command" next to the per-host directory |
| 8 | Mobile | One rounded rectangle at the bottom: New agent \| New terminal. Both open in `random` |
| 9 | ⌘⇧W, close all non-agent tabs | Already shipped (`window.closeNonAgent`, 7ebd0ca6, in 0.1.34). No work |

## 1. One pull request

The owner chose a single PR for everything below, including the gpui-kit
0.9 → 0.10 bump that CLAUDE.md would otherwise keep in a PR of its own. The
sections keep their A–D labels as areas of work:

| Area | Scope |
|---|---|
| A | Xcode 27 + Expo 58 / RN 0.88 + all JS dependencies + Rust lockfile |
| B | gpui-kit 0.9 → 0.10 |
| C | Desktop Keyboard tab, number-key setting, hold-⌘ badges, ⌘⇧N, per-host agent command, host `createWindow` directory |
| D | Mobile bar, per-host default directory |

The phone can meet an older host; §5 says what that host does.

## 2. PR A: toolchain and dependencies

### 2.1 Xcode 27

- `.github/workflows/ios.yml`: both jobs move to `runs-on: xcode-27` with
  `DEVELOPER_DIR: /Applications/Xcode_27.1.app/Contents/Developer`. 27.1 is
  the image default; 27.0 and 27.2 are also installed. The image is still
  marked preview by GitHub and is the only one with the iOS 27 SDK.
- The toolchain cache key already hashes `xcodebuild -version`, so caches roll
  over by themselves.
- That image switches the `openssl` CLI to OpenSSL 4. `check-native.sh` asks
  for `brew --prefix openssl@3` explicitly and pods use the prebuilt
  `OPENSSL_DOWNLOAD`, so nothing should change; the first CI run confirms it.
- `check-native.sh` compiles with `-Werror`: new clang warnings fail it.
  Fix the warning; do not drop `-Werror`.
- iOS build number 10. `docs/mobile/ios-plan.md` (≈580, 629, 653–661) is
  updated to Xcode 27 and SDK 58.

### 2.2 Expo SDK 58

- `expo` 57.0.16 → 58.0.26, then `pnpm exec expo install --fix` to align every
  `expo-*` and React Native library with SDK 58's `bundledNativeModules`:
  react-native 0.88.1, react 19.3.0, reanimated 4.7.1, screens ~4.30.1,
  webview 14.0.1 (a major: read its changelog), and the rest. Keep exact pins,
  as the repo does today. `babel-preset-expo`, `@types/react` and
  `react-test-renderer` follow.
- Minimum iOS stays 16.4: `ExpoModulesCore` 58 still declares 16.4, and RN
  0.88 needs 15.1.
- **Patches**
  - `expo-modules-jsi@57.0.5.patch`: delete it, its license copy and the
    `pnpm-workspace.yaml` comment. Upstream 58.0.23 moved
    `SWIFT_RETURNS_RETAINED` from the constructors to static `create`
    functions, which is the fix the patch waited for (expo/expo#49214).
  - `react-native-reanimated@4.5.1.patch`, the Android keyboard band left on
    screen after backgrounding mid-animation: not fixed upstream in 4.7.1, and
    the four files still exist. Port it to `react-native-reanimated@4.7.1.patch`
    and re-run the original reproduction on the API 35 emulator.
  - `@xterm/xterm@6.0.0.patch`: unchanged unless xterm moves.
- **Config plugins**
  - `withReleaseSigning.js` edits the Groovy `build.gradle` by regex and throws
    if the template changed. Check SDK 58's Android template; rewrite the plugin
    if the template moved or switched to Kotlin DSL.
  - `withLibssh2.js` and `withLocalNotifications.js`: confirm the Podfile line
    and the entitlements order still hold after prebuild.
- Local module `muxflow-ssh`: podspec `swift_version 5.9` compiles in Swift 5
  mode under Xcode 27's Swift 6 compiler. The libssh2 source build runs under
  the new clang.
- Other JS dependencies: `pnpm -r update --latest` for everything Expo does
  not pin (bufbuild, zustand, marked, dompurify, svg, safe-area and so on).
  `@bufbuild/protobuf` and `protoc-gen-es` regenerate `src/protocol/gen`;
  `protocol:check` gates it. The wire contract does not change.

### 2.3 Rust

`cargo update` (159 lockfile entries, all semver-compatible), then
`cargo deny check licenses`, `cargo test`, `cargo clippy`. gpui-kit stays at
0.9.0 here; it is PR B.

### 2.4 QA for PR A

Automated, all must pass: `mobile:check`, `mobile:test`, `protocol:check`,
`webview:check`, `check-native.sh`, `tests/release/test-build-apk.sh`, and the
simulator Maestro flows on the Xcode 27 runner (none-auth, recover,
generate-key, key-auth, setup-host, send-terminal, files-and-resume).

Manual, on three surfaces:

1. **Android emulator, on this machine (API 35).** I drive it end to end.
2. **iOS simulator, on CI.** A temporary Maestro walkthrough takes a
   screenshot of each screen below. I review every screenshot. The flow is
   removed before merge.
3. **Real iPhone.** An internal TestFlight build installed over build 9,
   checked against a 10-minute list by the owner: microphone, push
   notifications, screen awake, and upgrade in place.

The checklist, per surface where it applies:

- Upgrade in place: saved hosts, keys and preferences written by 57 read back
  in 58 (secure store).
- Connect, host key trust, key auth, reconnect after backgrounding.
- Agents and Workspaces tabs, badges, sort toggle, long-press sheet.
- Terminal: render, scroll, selection, copy, paste, composer, keyboard show
  and hide, background the app mid keyboard animation (the reanimated patch).
- Chat: messages, markdown, tool calls, attachments (image and document
  pickers), copy.
- Voice: record, playback rate, auto-play, playback with the screen off
  (Android), keep awake.
- Notifications: local notification arrives and opens the right pane.
- Files: browse, open, download, upload.
- History, Settings (font size, agent command), light and dark, splash and
  status bar.

Exit: all automated checks green, the checklist passes on all three surfaces,
no new warnings in the device logs.

## 3. PR C: desktop and host

### 3.1 Host: `createWindow` honours `directory`

`apps/host/src/service/tmux_actions/command.rs`, `configure_new_window`: when
`action.directory` is non-empty, pass
`-c escaped_format_literal(resolved_start_directory(&action.directory)?)`;
otherwise keep `#{pane_current_path}`. The checks match `new-session`:
absolute or `~`, exists, enterable, or the create is refused with the reason.
The proto comment on field 15 becomes "Where a created session's first pane,
or a created window, starts". There is no field change.

`createWindow` already answers with a pane id: the host fills it from the
postcondition snapshot (`tmux_actions.rs:273`). Nothing to add there.

Tests: a directory is honoured; a missing or relative directory is refused;
an empty one keeps the current pane's path.

### 3.2 Preferences

- `WorkspaceDefaults` (`crates/desktop-core/src/app_state.rs:247`) gains
  `agent_command: Option<String>` (`agentCommand`), validated like
  `startup_command` (`:475`), with a sample in
  `persistedAppState.contract.json`.
- `ShellPreferences` gains `number_keys: Option<NumberKeys>`
  (`Workspaces | Agents`), absent meaning Workspaces, with
  `skip_serializing_if` as `snooze_times` does.
- `Prefs` gains `number_keys` and carries `agent_command` in
  `workspace_defaults`. `set_workspace_defaults` takes the third field; its
  five callers are updated.

### 3.3 Settings › Workspace: Agent command

A third field below Startup command, built the same way as the other two:
`Input`, a hint, and a commit on Enter or blur through `commit_defaults`. It is
refilled when the active host changes. Placeholder "None". Hint: "What
New agent session in random (⌘⇧N) runs on this host."

### 3.4 Settings › Keyboard

- `Tab::Keyboard` after Agents; `TABS` becomes 8. `shortcuts.configure` opens
  Settings on this tab. The overlay dialog and its `open` are deleted; there
  is one editor.
- The body, top to bottom:
  1. "⌘1–9 jumps to": Workspaces | Agents (`radios`), writing `number_keys`.
  2. A toolbar: search (title, group or key glyphs), a "Changed" filter and
     "Restore all defaults".
  3. The commands, grouped as in the palette, each with its key caps or
     "Add shortcut", and a reset button when it differs from the default.
- Recording: clicking a row's keys focuses a recorder. `capture_key_down` on it
  takes the next chord. Escape cancels and stops propagation, so Settings does
  not close. Backspace removes the binding. Anything else goes through
  `shortcut_from_keystroke`.
  - A chord without ⌘, ⌃ or ⌥, or one GPUI cannot name
    (`gpui_keystroke` is `None`), is refused on the row.
  - A chord that collides shows on the row: "⌘E is also Toggle pane zoom.
    Swap · Replace · Cancel". Nothing is written until one is chosen.
  - While Settings is open, `on_keystroke` already ignores every chord
    (`keymap.rs:193`), so recording never runs a command.
- Commit model: each change is written at once with `set_overrides`, as every
  other Settings change is. The draft, Done and the `blocked` state go away.
  The startup collision repair stays.
- The two registry titles change to "Go to workspace or agent N" (ids stay
  `workspace.select1..9`, so overrides people saved keep working).

### 3.5 ⌘1–9 target

- One source of order: `agents::numbered_rows(cx) -> Vec<AgentListRow>`. It
  returns `display_order(rows(cx, sort, pinned_only).listed, sort)` with
  routable rows only and without the woken rows drawn above the list.
  `display_order` becomes `pub(crate)`. The sort and pinned-only state live in
  `Chrome` today; they are published to a small global, `AgentListOrder`,
  whenever Chrome changes them, so a command can read them without the view.
- `workspaces/commands.rs` `run`: for `workspace.select{n}`, when
  `number_keys == Agents`, open `numbered_rows[n-1]` with `agents::open_row`
  and focus its terminal (what a click does). Otherwise it does what it does
  today.
- Workspace rows keep their permanent numbers in Workspaces mode and lose them
  in Agents mode.

### 3.6 Numbers while ⌘ is held

- `Shell::render`'s root gets `on_modifiers_changed`. When the platform key
  alone is down (⌘ on macOS, Ctrl on Linux), start a 400 ms timer. When it
  fires, set a `ModifierHold` global to showing. Any other modifier state, any
  keystroke (cleared from `on_keystroke`), or the window going inactive clears
  it and drops the timer.
- In Agents mode, `agent_row` takes `number: Option<usize>`, drawn as a badge
  in the leading slot for the first nine `numbered_rows` while showing. In
  Workspaces mode nothing changes, since those numbers are always visible.
- 400 ms keeps ⌘C, ⌘V and Ctrl chords in the terminal from flashing badges.

### 3.7 ⌘⇧N: new agent session in `random`

- Registry: `agents.newInRandom`, "New agent session in random",
  `Meta+Shift+N` / `Ctrl+Shift+N`, `mutates()`. Both chords are unbound
  today. The registry count becomes 85 and the palette 67.
- `Workspaces::new_agent_in_random`:
  1. Host: the active host. Command: `defaults_for(host).agent_command`.
     None set: a notice "Set an agent command for this host in Settings ›
     Workspace", Settings opens on Workspace, nothing is sent.
  2. One at a time per host: a press while one is in flight is ignored.
  3. Look up a session named exactly `random` in that host's snapshot. Names
     are compared raw: session names are never glyph-stripped.
  4. Missing: `CreateSession { name: "random", directory, pinned:
     pinned_only }`, then type the agent command into the returned pane. The
     host's startup command is not typed here; the agent is the start.
     The notice "Created the random workspace" is shown.
  5. Present: `CreateWindow { session_id, directory }`, then type the agent
     command into the returned pane.
  6. Focus moves to the new tab (`commit_local`, as `create_workspace` does).
- Refactor: `create_workspace` and `create_window` share one path that takes an
  optional directory and an optional command. `send_startup` takes the failure
  wording as a parameter, so a window's failure does not say "Workspace
  created".

### 3.8 Docs

`docs/keybindings.md`: ⌘⇧N, the Keyboard tab replacing "Configure keyboard
shortcuts", and the number-key setting.

### 3.9 Tests

- Pure: registry counts and pinned keymap, `numbered_rows` per sort mode and
  pinned filter, prefs round trip of `number_keys` and `agent_command`, the
  core contract.
- GPUI (`tests/settings.rs`, `tests/commands.rs`): the Keyboard tab records,
  refuses an unsafe chord, swaps and replaces on conflict, removes, resets a
  row, restores all, filters by search; `shortcuts.configure` opens the tab;
  Escape while recording cancels the recording and leaves Settings open;
  eight tabs.
- `workspaces/tests.rs` with `FakeHost`:
  - `random` missing: `createSession` with the name and directory, input
    `codex\n` to `%9`.
  - `random` present: `createWindow` with the session and the directory, input
    to `%8`.
  - No agent command: no action, notice shown.
  - A second press while the first is in flight: one action.
  - A `random` on another host is not used.
  - Agents mode: ⌘3 opens the third numbered agent.
  - Hold: a modifier event, advance 400 ms, badges present; a keystroke clears
    them.
- Host: §3.1.

Manual QA on this Linux machine through the GUI driver: each flow above, plus
⌘1–9 in both modes with priority sorting while agents change state.

## 4. PR D: mobile

### 4.1 Default directory per host

`SavedHost` and `HostDraft` gain `defaultDirectory?: string` (`hostsStore.ts`:
type, draft, `parseHost`, `addHost`, `updateHost`). It is edited in Settings
under "New sessions" for the connected host, next to Agent command. Empty means
`~`.

### 4.2 Requests and flow

- `requests.ts`: `createSession(name, directory, identity, generation)`, and
  `createWindow` takes an optional `directory`.
- `createWindow.ts`: `openInRandom(connection, store, { kind, directory,
  command })`:
  1. Find `random` by exact name in `sessionStore.sessions`.
  2. Missing: `createSession`. Present: `createWindow` with the directory.
     Either way, retry once on `stale_topology`, as `createTerminalWindow`
     does.
  3. `kind === "agent"`: deliver the command with the same connection-scope
     checks as `createAgentWindow`.
  4. Return the pane and session ids.
- No agent command: the same toast and Settings jump that the sheet uses
  today.

### 4.3 The bar

`src/features/terminal/QuickCreateBar.tsx`:

- One rounded rectangle, 300 wide (never wider than the screen less 32),
  48 tall, radius 12, accent fill, 16 above the tab bar, centred.
- Two halves, "New agent" and "New terminal", with test ids `quick-new-agent`
  and `quick-new-terminal` and accessibility labels naming `random`.
- Shown on the Agents and Workspaces tabs. Disabled while not connected or
  while one is being created; the half that was pressed shows progress.
- After creation: `waitForPane` (3 s), then the terminal screen. "Created the
  random workspace" shows as a toast when the workspace was new.
- Both lists get 80 px of bottom padding, so the last row and its Talk button
  are never under the bar.

### 4.4 Tests

- Vitest: both request builders; `openInRandom` for missing, present, stale
  retry, no command and replaced connection; the hosts store round trip of
  `defaultDirectory`.
- Maestro: `quick-create.yaml` taps New terminal, lands in a terminal, goes
  back, and finds `random` in Workspaces.
- Manual on the Android emulator and the iOS simulator, with the PR A
  checklist's connection and terminal items repeated.

## 5. Wire changes

One: `TmuxAction.directory` (field 15) is now read for CREATE_WINDOW. There is
no new field and no new operation, so there is no `PROTOCOL_MAJOR` bump.

Desktop and host change in the same commit (CLAUDE.md).

For mobile, both directions:

- **Previous phone, new host:** the phone sends no directory on
  CREATE_WINDOW, so the window opens in the current pane's path, as today.
- **New phone, previous host:** creating `random` is correct (CREATE_SESSION
  already used the directory). A new window in an existing `random` opens in
  that workspace's current pane path instead of the default directory. That is
  the behaviour of New terminal today, it fails nothing, and it ends when the
  host updates.

## 6. Risk, confidence, complexity

| Work | Confidence | Risk | Complexity | What lowers it |
|---|---|---|---|---|
| A: Xcode 27 + Expo 58 + deps | 75 | 50 | 10 | The deleted JSI patch removes one unknown. Port reanimated first. Three-surface QA |
| A: Rust lockfile | 85 | 20 | 2 | CI and `cargo deny` |
| B: gpui-kit 0.10 | 60 | 40 | 5 | Not read yet; its changelog decides |
| C: Keyboard tab + recording | 85 | 25 | 12 | Reuses keymap functions and Settings helpers; deletes the overlay |
| C: number keys + hold badges | 85 | 25 | 8 | One `numbered_rows` source for badges and jumps |
| C: ⌘⇧N + host directory | 90 | 20 | 6 | Shares the create path instead of a new one |
| D: mobile bar | 85 | 20 | 10 | Same create-then-type pattern as the sheet |

## 7. Open points

- `react-native-webview` 13 → 14 is a major version. The changelog decides
  whether the terminal and chat webviews need changes.
- The `xcode-27` runner is in preview: queueing can be slow. No alternative
  image has the iOS 27 SDK.
