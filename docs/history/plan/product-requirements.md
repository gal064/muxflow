# tmux Agent IDE — Product Requirements

## Document status

- **Status:** implementation-ready v1 requirements
- **Initial audience:** personal/internal users
- **Future distribution:** open source
- **Desktop platforms:** macOS and Linux
- **Runtime:** tmux only

## Product context

Build a lightweight graphical IDE around an existing local or remote tmux server. The product is optimized for developers running terminal coding agents while wanting a modern file explorer, editor, Git diff client, file transfer workflow, and attention system.

tmux continues to own process persistence. The desktop application makes tmux sessions feel like graphical workspaces without exposing tmux's normal status bar, prefix-based controls, or copy mode.

Herdr defines the expected agent-oriented behavior: agent detection, working/blocked/idle state, unseen completion, rollups, sounds, and background notifications. The product adds graphical Explorer, Git, Monaco, Markdown, and remote transfer capabilities around that behavior.

## Goals

1. Attach to an existing local or remote tmux server without recreating its sessions.
2. Represent tmux sessions, windows, and panes as graphical workspaces, tabs, and splits in realtime.
3. Provide a graphical file explorer, editor, Markdown preview, and VS Code-like Git diff experience rooted at the active pane.
4. Make Codex and Claude Code sessions easy to launch, monitor, and revisit.
5. Support local-to-remote terminal uploads and remote-to-local downloads, including multi-gigabyte files.
6. Remain responsive and materially lighter than an Electron application.
7. Preserve normal tmux persistence and compatibility with other attached tmux clients.

## Non-goals

- Replacing tmux as the process/session runtime.
- Supporting Herdr as a backend.
- Building a complete VS Code replacement.
- Providing every Git workflow.
- Supporting more than one active host or tmux server at the same time.
- Nested tmux support.

## Product principles

- **tmux is authoritative:** the app reflects and controls tmux; it does not maintain a competing terminal topology.
- **Realtime by default:** external changes appear without refresh buttons.
- **Detach safely:** closing the app must not kill sessions or processes.
- **Agent attention is semantic:** distinguish working, blocked, completed, and merely idle agents.
- **One graphical mental model:** session = workspace, window = terminal tab, pane = split.
- **Local-feeling remote work:** SSH details should not leak into normal Explorer, Git, editor, notification, or transfer workflows.
- **Large data is streamed:** file size must not translate into equivalent application memory use.

## Terminology and hierarchy

```text
Host
└── tmux session = workspace
    ├── tmux window = terminal tab
    │   └── tmux pane = terminal split
    ├── file/Markdown tab = app-owned editor tab
    └── Git diff tab = app-owned diff tab
```

- **Host:** either the local machine or one SSH target.
- **Workspace:** one tmux session shown in the vertical workspace rail.
- **Terminal tab:** one tmux window.
- **Editor tab:** an app-owned file, Markdown, or diff tab belonging to a workspace.
- **Active root:** the active pane's Git root, or its current working directory when not inside Git.
- **Agent done:** an agent that is idle after completing work and whose completion has not yet been seen.

## Supported environment

### Desktop clients

- macOS on Apple Silicon and Intel where practical for internal distribution.
- Linux x86-64 and ARM64.
- Windows is not required for v1.

### Hosts

- Local macOS or Linux.
- Remote Linux over OpenSSH.
- tmux 3.3 or newer.
- Git installed for Git features.
- One tmux server per active host in v1.

### Authentication

- Use the user's existing OpenSSH configuration, known-hosts, keys, and ssh-agent.
- Do not collect or store SSH passwords or private keys.
- Remote helper installation runs as the SSH user and requires no root access.

## Application layout

### Workspace rail

- A vertical list shows every tmux session on the active server.
- New sessions created externally appear automatically.
- Closed and renamed sessions update automatically.
- Each workspace shows rolled-up agent attention and unseen state.
- The user can create, select, rename, reorder, and close workspaces.

### Explorer/Git sidebar

- Explorer and Git share one sidebar surface and are switched using icons/tabs.
- Both follow the active root.
- Changing panes or running `cd` can change the root without closing existing editor tabs.

### Main tab strip

- Each workspace owns its own tab set.
- tmux windows appear as terminal tabs.
- Files, Markdown documents, and Git diffs appear as app-owned tabs.
- Renaming a terminal tab renames its tmux window.
- Reordering terminal tabs reorders tmux windows.
- Editor tabs restore after restarting the desktop app.

### Terminal splits

- A selected terminal tab renders every tmux pane as a graphical split.
- tmux remains responsible for the actual pane layout.
- Split focus, resize, close, and zoom stay synchronized with external tmux clients.

### Agent sidebar

- A collapsible right sidebar lists agents across workspaces.
- Default ordering prioritizes attention and then recent activity.
- Selecting an agent focuses its workspace, terminal tab, and pane.
- State is also rolled up onto terminal tabs and workspace rows.

## Functional requirements

### PR-1: Host connection

1. The user can choose local mode or one saved SSH host profile.
2. Only one host is active at once.
3. Connecting must discover an existing tmux server and all sessions.
4. The app must not silently create a different tmux server when an expected server is unavailable.
5. Closing the app detaches only.
6. Connection loss leaves the last UI visible but read-only.
7. The app reconnects automatically and performs a complete state reconciliation.
8. Mutating actions are not queued during disconnection.

### PR-2: tmux workspace management

1. External session/window/pane create, rename, focus, resize, reorder, and close operations update the app live.
2. GUI operations update tmux immediately.
3. V1 supports:
   - Create, rename, reorder, select, and close sessions.
   - Create, rename, reorder, select, and close windows.
   - Split right/down, focus, resize, zoom, and close panes.
4. Killing a session, window, or pane requires confirmation.
5. Moving an individual pane between windows is not supported in v1.
6. A normal tmux client may remain attached simultaneously.
7. The app must not rewrite user tmux status, prefix, or keybinding configuration.

### PR-3: Terminal experience

1. tmux's status bar and copy mode are not displayed by the app.
2. The app provides its own terminal rendering, scrollback, selection, copy, search, links, and scrollbar.
3. Terminal rendering supports agent TUIs, shells, full-screen programs, Unicode, colors, mouse reporting, bracketed paste, and alternate screen.
4. New output does not force the viewport to the bottom while the user is scrolled up; show a new-output indicator.
5. A modifier forces local text selection when a TUI has mouse reporting enabled.
6. Normal application shortcuts control outer tmux without a prefix.
7. macOS defaults include:
   - `Cmd+N`: new workspace/session.
   - `Cmd+T`: new terminal tab/window.
   - `Cmd+W`: close current app-owned tab or confirm-close the terminal window.
   - `Cmd+D`: split right.
   - `Cmd+Shift+D`: split down.
8. Linux uses configurable conventional defaults, generally `Ctrl+Shift` combinations.

### PR-4: Nested tmux — unsupported

Nested tmux is unsupported and is not a release requirement. Running `tmux`
inside a managed pane is ordinary terminal content; the app does not discover,
route, label, or control an inner tmux server.

### PR-5: Active root

1. Explorer and Git follow the active tmux pane.
2. Inside a repository, the root is the Git worktree root.
3. Outside a repository, the root is the pane's current working directory.
4. Running `cd` updates the root automatically.
5. Existing file/diff tabs remain open when the root changes.
6. Rapid pane/root changes must not display stale results from an older root.

### PR-6: File Explorer

1. Show files and directories lazily and update them live.
2. Show dotfiles and Git-ignored files.
3. Display `.git` and `node_modules` as collapsed entries, but do not recursively traverse or watch them by default.
4. Never traverse `.git` internals.
5. Do not automatically follow directory symlinks.
6. Support create, rename, move, duplicate, and delete.
7. Confirm non-empty directory deletion and destination overwrite.
8. Show file type, size, modified time, and symlink state where useful.
9. Provide remote `Download…` and `Download Folder…` actions.
10. Dragging remote files out into Finder/a Linux file manager is not required in v1.

### PR-7: Editor and Markdown

1. Open text files in Monaco.
2. Auto-save approximately 150 ms after editing stops.
3. Reload open files when they change externally, including agent edits.
4. Last writer wins; v1 has no edit-conflict resolution UI.
5. Show saving and save-error state.
6. Default editor limit is 10 MiB for text; larger content remains downloadable.
7. Binary files show metadata and download actions rather than Monaco.
8. Images up to 25 MiB may be previewed.
9. Markdown supports source, preview, and split modes with continuous rendered updates.
10. Markdown WYSIWYG editing is not required in v1.
11. Restore open editor/diff tabs by host and workspace after app restart.

### PR-8: Git client

1. Display staged, unstaged, untracked, renamed, deleted, binary, and conflict states.
2. Open staged and unstaged changes in a VS Code-like Monaco diff view.
3. Support stage, unstage, and discard for complete files.
4. Support stage, unstage, and discard for complete hunks.
5. Selected-line operations are not required.
6. Every discard requires confirmation.
7. Support committing with a message and display Git hook/errors clearly.
8. Refresh after filesystem changes and Git mutations.
9. Handle initial repositories, worktrees, submodules as entries, renames, deletes, and conflicts.
10. Branch operations, fetch/pull/push, merge/rebase, stash, and history are out of scope.

### PR-9: Agent launch and detection

1. V1 supports Codex and Claude Code only.
2. The app can detect agents started manually.
3. The app can launch either agent into a new tmux window or pane using the active root.
4. The architecture must allow future adapters without changing generic UI/state behavior.
5. Hooks are the sole source of lifecycle state. A pane whose hooks are not installed reads `unknown`.
6. Process detection proves presence only: it discovers agents and retires them when they exit, and never reports what an agent is doing. Terminal-screen detection was removed — it could only see panes the user had on screen, and it matched vendor TUI output that changes without notice.
7. Remote hook events must cross the existing SSH connection; they must not contact the desktop through a public port.
8. Hook installation is explicit, reviewable, idempotent, and must preserve unrelated user configuration.

### PR-10: Agent lifecycle and attention

Support the Herdr-style semantic lifecycle:

- **Working:** actively processing a turn or performing work.
- **Blocked:** needs permission, input, or another user decision.
- **Idle:** not currently working.
- **Unknown:** agent detected but state cannot be classified reliably.
- **Done:** idle after completion with an unseen completion generation.

Requirements:

1. State rolls up agent → pane → terminal tab → workspace.
2. Blocked outranks working; unseen completion remains visible until seen.
3. Focusing the relevant pane marks its current attention generation seen.
4. Hook state must not be overwritten immediately by a weaker heuristic.
5. State survives SSH reconnect through a current snapshot.
6. Old events replayed after reconnect must not create duplicate notifications.
7. Agent names may be user-renamed independently of file/editor tabs.

### PR-11: Native notifications

1. Use native macOS and Linux notifications, not browser notifications.
2. Notify when a background agent becomes blocked.
3. Notify when a working background agent completes and becomes idle/done.
4. Suppress redundant native notifications for the actively focused pane.
5. Play configurable sounds.
6. Notification content defaults to agent, state, workspace, and terminal tab; do not reveal prompt/output content on lock screens.
7. Clicking a notification must:
   - Launch/focus the app.
   - Reconnect when needed.
   - Select the originating workspace, terminal tab, and pane.
   - Mark the current attention generation seen.
8. Renames must not break routing.
9. If the destination was closed or belongs to another server identity, do not
   focus a substitute pane; explain that the exact target no longer exists and
   preserve its in-app attention state as unseen.
10. On Linux notification daemons without click actions, preserve a native notification and in-app attention state even though click routing is unavailable.
11. Native notifications are guaranteed while the desktop application is running or reconnecting; background notifications after full app exit are not required in v1.

### PR-12: Terminal uploads and clipboard paste

1. Dropping/copying local files into a remote terminal uploads them to a private remote staging directory.
2. Paste the resulting remote path only after upload verification completes.
3. Do not automatically press Enter.
4. Local files dropped into a local terminal normally paste their existing paths.
5. Clipboard images are encoded as PNG, staged, and pasted as an agent-recognizable path.
6. Clipboard images have a 25 MiB hard limit after encoding.
7. Non-image paths are shell-escaped; image paths use the validated agent-compatible paste behavior.
8. Multiple files are supported; directory drop is deferred.
9. Dragging into Explorer as an upload mechanism is not required.

### PR-13: Upload/download limits and behavior

1. Regular uploads and downloads have no fixed application size cap.
2. Use 64-bit sizes/offsets and bounded streaming; never buffer the entire file.
3. Confirm terminal uploads larger than 500 MiB.
4. Preflight source access, destination collision, permissions, and available space.
5. Show progress, transferred bytes, throughput, estimated time, cancellation, and errors.
6. Limit concurrent bulk transfers to two and queue the rest.
7. A failed/cancelled transfer must not expose a complete-looking destination file.
8. Verify final byte count and content digest before completion.
9. Clean up app-owned partial files after failure/cancellation where possible.
10. Use a private home-cache staging directory rather than `/tmp`, which may be memory-backed.
11. Transfer resume is not required in v1.
12. A 5 GiB upload and download are manual release acceptance tests.

### PR-14: Persistence and recovery

1. tmux owns terminal persistence; app exit only detaches.
2. The app persists host profiles, window geometry, sidebar state, shortcuts, and app-owned tabs locally.
3. App restart must not recreate existing tmux processes.
4. Reconnect replaces stale tmux/file/Git/agent caches with authoritative snapshots.
5. A restarted tmux server must not cause old pane IDs to be associated with new processes.
6. Incompatible remote-helper versions place the UI in read-only mode and offer an explicit upgrade.

## Realtime and performance requirements

The release target is:

- 20 tmux sessions.
- 100 tmux windows.
- 50 live panes.
- A repository containing 250,000 files.
- A remote host with 100 ms round-trip latency.

Target observable latency:

- Topology changes: 250 ms local / 500 ms remote after receipt.
- Active-root changes: 500 ms local / 1 second remote.
- Normal Explorer events: 500 ms local / 1 second remote.
- Git status: normally within 1 second after changes settle.
- Terminal repaint: fluid 60 Hz under normal output; bounded degradation and recovery under floods.
- File-transfer memory: bounded independently of file size.
- A bulk transfer must not materially interrupt interactive typing or agent notifications.

## Safety, privacy, and destructive actions

- Never store SSH secrets.
- Use user-only permissions for local/remote sockets, state, and staging files.
- Confirm session/window/pane kills, non-empty directory deletion, overwrite, Git discard, and uploads over 500 MiB.
- Do not execute mutations queued before a reconnect.
- Sanitize Markdown and untrusted links.
- Gate terminal clipboard writes and external URL opening.
- Keep terminal contents, prompts, file content, and credentials out of notifications and ordinary logs.
- Clearly document that Git discard and last-writer-wins file races may be unrecoverable without Git history or backups.

## V1 exclusions

- Herdr runtime/backend.
- Windows.
- Multiple simultaneous hosts or tmux servers.
- Native libghostty or ghostty-web.
- Nested tmux support or topology/agent integration.
- Pane movement between windows.
- LSP/completion/diagnostics/debugging.
- Editor extension marketplace.
- Advanced Git/network/history workflows.
- Selected-line staging.
- Markdown WYSIWYG.
- Directory drop into terminals.
- Remote drag-out.
- Resumable transfers.
- Notification delivery after the desktop app fully exits.
- Concurrent-edit conflict UI.

## Release acceptance scenarios

1. Attach to an existing local tmux server and display every session/window/pane correctly.
2. Mutate topology from both the app and a normal tmux client; both reconcile without configuration changes.
3. Close/reopen the app; all tmux processes survive and app-owned tabs restore.
4. Work through 100 ms SSH latency, force a disconnect, reconnect, and recover without duplicated output or mutations.
5. Run Codex and Claude Code locally/remotely and verify working, blocked, idle/done, unseen, rollups, sounds, and suppression.
6. Click native notifications on macOS/Linux and focus the exact destination, including after rename and reconnect.
7. Create/edit/delete files externally and in Monaco; Explorer, Markdown, editor models, and Git update automatically.
8. Stage/unstage/discard files and hunks and commit with results matching Git CLI.
9. Upload and download verified 5 GiB files with bounded memory and responsive terminal input.
10. Paste local files and images into remote agents and receive correct remote paths without an automatic Enter.

The former scenario 11 was withdrawn from V1 on 2026-08-11. V1 has ten release
acceptance scenarios; the unsupported feature is not a release gate.

## Final product decision summary

Version 1 is a Tauri desktop application using xterm.js/WebGL and Monaco, backed by a shared Rust local/remote host core. tmux is the only multiplexer. Herdr is the behavioral baseline for agents and notifications. All critical state is live and reconciled. Large transfers are streamed with no fixed regular-file cap. The implementation is complete only when local and remote macOS/Linux workflows satisfy the release scenarios above.
