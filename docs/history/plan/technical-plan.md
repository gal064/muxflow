# tmux Agent IDE — Technical Implementation Plan

## Context

This document implements [product-requirements.md](./product-requirements.md). It assumes those requirements are authoritative and records only the recommended technical approach.

Build a Tauri 2 desktop application for macOS/Linux with a React/TypeScript system-WebView UI, xterm.js/WebGL terminals, Monaco editors/diffs, and a Rust local/remote host runtime. tmux is authoritative for persistent terminal topology. A private Rust daemon supplies the structured tmux, filesystem, Git, transfer, and agent event layer that tmux itself does not provide.

## Architecture

```text
macOS/Linux desktop
┌──────────────────────────────────────────────────────────┐
│ Tauri native process                                     │
│ ├── React/TypeScript system WebView                      │
│ │   ├── xterm.js/WebGL terminal surfaces                 │
│ │   ├── Monaco editor/diff                               │
│ │   └── workspace/Explorer/Git/agent UI                  │
│ ├── native notifications, menus, dialogs, clipboard      │
│ └── connection/state coordinator                         │
└───────────────────────┬──────────────────────────────────┘
                        │ versioned binary protocol
            local UDS   │   or SSH stdio bridge
                        ▼
┌──────────────────────────────────────────────────────────┐
│ tmux-ide-host (local or remote, user-owned)               │
│ ├── tmux -CC control clients                             │
│ ├── file/CWD watchers and file operations                │
│ ├── Git CLI adapter                                      │
│ ├── Codex/Claude hook receiver and detector              │
│ └── transfer service                                     │
└───────────────┬───────────────────────────────┬──────────┘
                │                               │
                ▼                               ▼
         existing tmux server             host filesystem/Git
```

No public listener is created. Remote hooks report to a user-only Unix socket; the daemon forwards state through the authenticated SSH control lane.

## Technology stack

### Desktop

- Tauri 2 and Rust stable.
- React and TypeScript with Vite.
- xterm.js plus WebGL, fit, search, links, Unicode, and ligature addons.
- Monaco editor and diff editor.
- A small reducer-oriented state store; select the lightest library that supports normalized entities and ordered event application.
- Native macOS notification integration through the User Notifications framework.
- Native Linux notifications through `org.freedesktop.Notifications` over D-Bus.

### Rust host/core

- Tokio asynchronous runtime.
- `prost`/Protobuf for generated forward-compatible protocol messages.
- `notify` for filesystem events with polling fallback.
- Host Git CLI via Tokio processes; do not use libgit2.
- BLAKE3 for streamed transfer integrity.
- `tracing` for structured, redacted logs.
- Property/fuzz tests for byte protocols and parsers.

### Terminal choice

xterm.js/WebGL is the locked v1 renderer. Do not implement a libghostty or ghostty-web branch. Keep the renderer behind a narrow TypeScript interface so terminal state/write/resize/input/selection/search lifecycles are testable without leaking xterm objects through application state.

## Repository layout

```text
Cargo.toml
package.json
pnpm-workspace.yaml
apps/
  desktop/
    src/
      app/                         # composition and routing
      commands/                    # one registry for menus/palette/keys
      components/
      features/
        agents/
        editor/
        explorer/
        git/
        terminal/
        transfers/
        workspaces/
      state/                       # snapshot/event reducer and selectors
    src-tauri/
      src/
        app_state.rs
        commands.rs
        connection.rs
        notifications/
          macos.rs
          linux.rs
        persistence.rs
      capabilities/
      tauri.conf.json
  host/
    src/
      main.rs
      daemon.rs
      bridge.rs
      hook.rs
crates/
  protocol/
  tmux-control/
  host-services/
  agent-runtime/
fixtures/
  agent-hooks/
  terminal-streams/
  tmux-control/
tests/
  e2e/
  integration/
```

Avoid splitting additional crates until an ownership or dependency boundary is demonstrated. Local and remote modes must execute the same host-service code and protocol.

## Core domain model

Use stable runtime identities rather than names:

- `HostId`: saved profile plus resolved tmux socket/server identity.
- `SessionId`: tmux `$` ID, with name as display/fallback metadata.
- `WindowId`: tmux `@` ID.
- `PaneId`: tmux `%` ID.
- `EditorTabId`: app UUID scoped to host/session.
- `AgentId`: adapter kind plus native session ID where available; otherwise a daemon-generated stable lifetime ID.
- `Generation`: monotonic counter for snapshots, attention state, file versions, and ordered host events.

Persist names and fallback matching only for app-owned tab restoration. Never assume IDs survive a tmux server restart; include a server-instance identity and rebuild all runtime associations when it changes.

## Host daemon and transport

### Process roles

The same binary exposes:

- `tmux-ide-host daemon`: persistent user process and private Unix socket.
- `tmux-ide-host bridge --stdio`: SSH/local stdio adapter to the daemon socket.
- `tmux-ide-host hook ingest`: low-overhead Codex/Claude hook endpoint.
- `tmux-ide-host doctor`: read-only prerequisite and diagnostics report.

Daemon runtime/state directories must be user-only. The hook entry point should send to the socket and exit quickly; if the daemon is momentarily unavailable, store only a bounded, atomic latest-state fallback that the daemon ingests on restart.

### Protocol

Use length-delimited Protobuf envelopes containing:

- Protocol and helper versions.
- Request/response ID.
- Event sequence/generation.
- Stream ID and priority.
- Snapshot/event/control/transfer payload variants.
- Raw bytes for terminal output and file chunks.
- `u64` sizes and offsets.
- Structured error code plus safe display text.

Handshake publishes OS/architecture, tmux/Git versions, server identity, and capability bits. Major incompatibility leaves the desktop read-only and offers helper upgrade. Unknown optional fields/features must be ignored safely.

Snapshots bootstrap each domain. Ordered events mutate cached state. A missing sequence or queue overflow requests a scoped snapshot; reconnect always fetches a complete snapshot before enabling mutations.

Do not use Tauri JSON events for terminal output. Feed bounded native Tauri channels into a frontend binary-frame dispatcher, then batch pane writes per animation frame.

### SSH connection management

- Invoke the system OpenSSH client so the user's config, ProxyJump, known-hosts, keys, and agent work unchanged.
- Create a private temporary OpenSSH control socket after including user/system config.
- Use one persistent control connection and at most two bulk-transfer SSH connections. Bulk transfers must use separate TCP connections (`ControlMaster=no`, `ControlPath=none`) rather than multiplexing over the control master; the Phase 0 saturated-link spike reduced worst measured control latency from about 312 ms to 173 ms by separating them.
- Preserve OpenSSH host-key/auth prompts in an app-owned connection surface or clear error path.
- Never parse/copy private credentials.
- Reconnect the control lane with exponential backoff and jitter; cancel pending mutations immediately on loss.

### Remote helper install/upgrade

1. Probe remote OS/architecture and existing helper through SSH.
2. Require Linux x86-64 or ARM64, tmux 3.3+, and Git for Git features.
3. Prompt before installation/upgrade under `~/.local/bin`.
4. Upload a matching artifact to a temporary path.
5. Verify expected digest and architecture.
6. Apply executable permissions and atomically replace the managed binary.
7. Start/reuse the user daemon without requiring systemd or root.
8. Keep hook install/upgrade as a separate, explicit action.

## tmux control implementation

### Discovery and topology

- Resolve the intended tmux socket explicitly.
- Use one `tmux -CC` client per session because control mode sends pane output for its attached session.
- Parse `list-sessions`, `list-windows`, and `list-panes` with explicit `-F` formats.
- Track `%sessions-changed`, session/window rename/focus/add/close, pane/layout, and exit notifications.
- External changes only mark topology dirty. A coalesced reconciliation pass queries fresh authoritative state and produces an entity diff.
- Only the visible control client submits a size claim; background clients do not participate in sizing.
- Configure `pause-after` flow control and recover paused panes through continue plus scoped screen resnapshot.

### Byte parser

Implement an incremental parser that:

- Preserves arbitrary bytes until a complete control record is recognized.
- Correlates `%begin`/`%end`/`%error` command blocks.
- Parses async notifications while commands are outstanding.
- Parses and octal-unescapes `%output` before any UTF-8 conversion.
- Retains partial UTF-8 and terminal escape sequences across records.
- Rejects oversized/malformed control lines with a deterministic resnapshot/error policy.
- Never blocks output consumption while awaiting a command result.

Add fuzz targets and fixtures for non-UTF-8, split escape sequences, large output, malformed octal, command interleaving, disconnect boundaries, and old supported tmux versions.

### Screen seed/replay

`%output` is append-only and does not provide a complete current screen. For each fresh/reconnected pane:

1. Begin buffering live output for that pane.
2. Query pane grid/history/alternate-screen metadata.
3. Run `capture-pane -p -e -S <history-limit>`.
4. Produce an ANSI seed that paints history, visible rows, cursor, and available mode state.
5. Write the seed into a fresh xterm model.
6. Replay bytes buffered after the seed boundary.
7. Continue live streaming.

Persist bounded serialized xterm snapshots and raw tails for recently hidden panes. Dispose WebGL renderers for long-hidden tabs while preserving recoverable terminal state. Resnapshot on flow-control pause, parser gap, tmux server replacement, or failed parity check.

Phase 0 must prove shells, Codex, Claude Code, Neovim/Vim alternate screen, mouse TUIs, Unicode/graphemes, OSC links, wraps, cursor motion, resize, and output-during-capture. Treat this as a release-blocking technical foundation.

### Input

- Frontend command handling intercepts app shortcuts before xterm input.
- Send remaining keyboard/paste/focus/mouse/query-reply bytes with addressed `PaneId`.
- Daemon emits literal bytes through control-mode `send-keys -H` in small bounded batches without waiting per keystroke.
- Preserve bracketed paste and child mouse protocols.
- Never implement outer tmux prefix handling.

### Layout and resize reconciliation

- xterm cell metrics determine the available window grid.
- Send `refresh-client -C` only from the visible session's client.
- Parse tmux layout strings into a tree and map cell rectangles into UI split geometry.
- User divider drag sends `resize-pane`; the next tmux layout is authoritative.
- Use a dirty generation per mirrored window. Events update durable facts and increment it. A pass snapshots all facts, computes desired state, diffs actual state, commits requests/view changes, and schedules exactly one follow-up if the generation changed during the pass.
- Respect other attached clients and user tmux window-size behavior; never change their flags or global options.

## Desktop UI implementation

### State flow

- One normalized frontend store receives a full host snapshot and ordered domain events.
- Separate tmux-authoritative entities from app-owned editor tabs/preferences.
- Event reducers validate server identity, sequence, and generation before mutation.
- Selectors derive workspace rollups, tab contents, split layout, active root, and notification destination.
- A disconnected state freezes mutations while retaining the last snapshot for inspection.

### Commands

Create one typed command registry. Menus, command palette, title-bar actions, context menus, and shortcuts invoke the same command IDs and precondition/confirmation logic. Provide platform defaults and user overrides.

### Workspace surfaces

- Far-left vertical session rail with agent attention badges.
- One adjacent sidebar switching Explorer/Git.
- Combined session tab strip containing tmux windows and app-owned file/Markdown/diff tabs.
- Terminal window content mirrors tmux splits.
- Collapsible Herdr-style agent sidebar on the right.
- Persist only app-owned state and UI preferences; tmux topology always comes from the host.

### Terminal renderer adapter

Define a TypeScript `TerminalRenderer` interface for create, write bytes, seed/restore, resize, focus, selection, search, serialize, dispose GPU renderer, and input event subscriptions. Implement it only with xterm.js. Keep xterm instances out of the global serializable store.

Batch writes by pane on `requestAnimationFrame`, cap per-frame work, and yield under floods. Mount WebGL only for visible panes; fall back to xterm's non-WebGL renderer with a visible diagnostic if WebGL initialization fails.

### Nested tmux

Nested tmux is outside V1 support and release acceptance. Running `tmux`
inside a managed pane is ordinary
terminal content. Phase 9 removed the former detection, mapping, marker,
protocol, and UI machinery.

## Active root, files, and editor

### Root tracking

- Use tmux `#{pane_current_path}` and foreground-process CWD information.
- Poll only the active pane at a short interval because tmux does not emit a dedicated CWD event.
- Cancel stale probes after focus changes.
- Resolve Git worktree root asynchronously; otherwise keep CWD.
- Publish one atomic root-change event consumed by Explorer and Git together.

### Explorer service

- Lazy `list_directory` RPC with stable entry metadata.
- Native watcher for active/expanded directories; bounded polling fallback.
- Debounce/coalesce ordinary bursts; scoped resnapshot on overflow.
- Show ignored/dotfiles.
- Do not recurse into `.git`, unexpanded `node_modules`, or directory symlinks.
- Validate create/rename/move/duplicate/delete paths against the requested operation and re-fetch affected parent directories after mutations.
- Confirm non-empty directory delete and overwrite in the desktop before sending the mutation.

### Monaco and Markdown

- Create one Monaco model per open file path/session and preserve view state per tab.
- Default text limit 10 MiB and image preview limit 25 MiB.
- Debounce writes 150 ms; expose saving/error state.
- Use write operation IDs plus host file generations to suppress exact self-echo while still accepting later external changes.
- On any newer external generation, replace the model immediately by product decision; no merge/conflict UI.
- Preserve permissions and symlink behavior on save.
- Render Markdown from the current model through a sanitized pipeline and source/preview/split modes.
- Persist host/session/path/tab kind/order/selection in a versioned app-state file; reopen missing files explicitly as missing.

## Git implementation

- Invoke the host Git CLI with explicit `-C <root>` and non-interactive environment.
- On Darwin, use the mainstream desktop/path-based local trust boundary: derive
  absolute worktree, Git-dir, and common-dir paths from held directory
  descriptors; verify device/inode identity during command construction,
  immediately before child `exec`, and after completion; serialize mutations
  and fail/refresh on identity change. A hostile same-user race inside stock
  Git's own pathname opens is outside the desktop threat model and would require
  a patched Git or privileged mediation.
- Parse `git status --porcelain=v2 -z` from bytes.
- Coalesce watcher events and cancel superseded reads.
- Fetch base/index/worktree contents for Monaco diff models.
- Serialize mutating operations per repository.
- Stage/unstage files with path-safe argument passing.
- Generate complete-hunk patches and apply with Git, including reverse/cached variants; reject a mutation if the source diff generation changed.
- Confirm discard in the UI and refresh status/diff afterward.
- Commit with user message and surface stdout/stderr/hook errors.
- Represent conflicts/binary files safely without attempting unsupported edits.
- Test initial repositories, worktrees, submodules, rename/delete, ignored/untracked, conflicts, hooks, and unusual filenames.

## Agent runtime

### Normalized state

Implement `working`, `blocked`, `idle`, and `unknown`. Represent `done` as `idle` plus an unseen completion generation. Each record carries adapter, native session ID, exact pane/session/window IDs when mapped, timestamps, and seen generation. Hooks are the only writer of lifecycle, so a record does not name a source; a pane with no hooks stays `unknown`.

Roll up attention agent → pane → window → session. `blocked` outranks `working`; unseen completion remains until its destination is focused.

### Adapter contract

`AgentAdapter` owns:

- Process identification.
- Launch/resume command.
- Hook config and event parser.
- Session identity extraction.
- Lifecycle normalization.
- Hook-lease expiry.
- Process detection rules (presence and retirement only).

Implement Codex and Claude Code only. New Agent actions select adapter plus new-window/new-split placement and inherit active root.

### Hook management

- Install labeled managed entries only after confirmation.
- Merge with existing Codex/Claude config and preserve unrelated hooks.
- Back up touched config and provide idempotent upgrade/uninstall.
- Respect Codex hook review/trust instead of bypassing it.
- Hook command sends JSON to `tmux-ide-host hook ingest` and inherits `TMUX_PANE`.
- Daemon accepts a pane only when hook-origin server identity and exact pane ID match the active topology; otherwise it remains unmapped. It applies monotonic generation, stores compact latest state, and emits events.
- Reconnect snapshot updates UI without replaying native notifications for old generations.

Use Herdr's Apache-licensed manifests/state semantics as a narrow attributed reference where useful. Do not depend on Herdr. Treat cmux GPL and Warp AGPL code as design-only unless the project license changes deliberately.

## Native notifications

### macOS

- Implement a Tauri/Rust native module using the User Notifications framework.
- Request permission in-context after explaining agent notifications.
- Store route metadata in notification user info.
- Delegate click/action callbacks into the desktop connection coordinator.

### Linux

- Use D-Bus `org.freedesktop.Notifications`.
- Send a default click action where server capabilities allow.
- Listen for `ActionInvoked` and map notification IDs to route metadata.
- Fall back to non-clickable native notification plus persistent in-app attention when actions are unavailable.

### Routing

Route contains host profile, server identity, session/window/pane IDs and name fallbacks, agent ID, and attention generation. On activation:

1. Focus/launch the app.
2. Connect/reconnect the saved host.
3. Fetch/validate current topology.
4. Resolve the exact server and pane IDs; renames do not change those IDs.
5. Focus workspace/window/pane and xterm.
6. Mark only the matching generation seen.
7. If closed or foreign, focus nothing, report the route unavailable, and
   preserve the in-app attention item as unseen.

Notify only background blocked transitions and background working→idle completion transitions. Suppress focused-pane duplicates and avoid sensitive prompt/output text.

## Transfer engine and terminal paste

### Lanes and flow

- Start at most two bulk SSH sessions on independent TCP connections; never send file bodies or bulk sessions through the terminal control master's TCP connection.
- Use bounded chunk queues and `u64` offsets.
- Control lane never carries large file bodies.
- Cancellation propagates to source and destination and closes the bulk channel predictably.

### Integrity and completion

1. Preflight source, destination, collision policy, permissions, and free space.
2. Confirm terminal uploads above 500 MiB.
3. Open a private UUID `.partial` destination.
4. Stream chunks while calculating BLAKE3 at both ends.
5. Report progress/speed/ETA through throttled events.
6. Verify total bytes and digest.
7. Flush, set intended permissions, and atomically rename.
8. Remove owned partial files on cancellation/failure and report cleanup failure.

Use `~/.cache/<app>/uploads` with `0700` directories and `0600` files for remote terminal staging. Implement age/size cleanup only for app-owned files and never delete active-transfer paths.

### Drop/clipboard behavior

- Remote local-file drop: upload, verify, then bracketed-paste remote path without Enter.
- Local-host file drop: paste existing escaped path.
- Clipboard image: encode PNG, reject over 25 MiB, stage, and paste agent-compatible path.
- Shell-escape non-image paths; preserve validated raw image-path semantics required by Codex/Claude.
- Multiple files are sequentially/parallel uploaded within the two-transfer limit, then paths are pasted in deterministic order.
- Directory terminal drop and remote drag-out remain absent.

## Persistence, logging, and security

### Persistence

- tmux persists terminal runtime.
- Desktop persists versioned app-owned JSON/state: profiles, geometry, preferences, commands, and editor/diff tabs.
- Host persists only helper version/config, compact agent latest state, transfer ownership metadata, and operational state required for reconnect.
- Atomic writes and schema migrations are mandatory.

### Logging/diagnostics

- Structured logs with subsystem, IDs, generations, timings, and error classes.
- Redact terminal bytes, prompt text, file content, credentials, and notification-private content.
- Provide an explicit diagnostics bundle with versions, feature capabilities, queue/flow counters, and redacted recent errors.
- No telemetry upload in v1.

### Security

- User-only daemon socket/runtime/staging permissions.
- OpenSSH handles authentication and host verification.
- Validate frame lengths, offsets, paths, and archive entries.
- Prevent cleanup/path traversal and symlink escape outside app-owned staging.
- Sanitize Markdown and URLs; gate OSC clipboard/external opening.
- Confirm destructive tmux/file/Git operations and large uploads.
- Never replay queued mutations after reconnect.
- Hook installation is reviewable, reversible, and scoped.

## Implementation phases

### Phase 0 — Risk spikes and scaffolding

1. Scaffold Rust workspace, Tauri/React app, protocol generation, lint/test/CI on macOS/Linux.
2. Discover a local tmux session and render one pane in xterm.js/WebGL.
3. Implement capture/buffer/replay and validate alternate screen, Unicode, cursor, wrap, resize, and output during seed.
4. Render one tmux window's splits; resize and reconcile to tmux.
5. Attach an ordinary second tmux client at a different size and verify coexistence.
6. Repeat through SSH at 100 ms simulated RTT and recover a forced disconnect.
7. Prove native notification click routing on macOS/Linux with synthetic topology.
8. Stream a large synthetic file on a bulk lane while typing on the control lane.

**Must-pass gate:** terminal reconstruction, input, layout reconciliation, external-client coexistence, SSH recovery, notification routing, and lane isolation pass on both desktop platforms before feature expansion.

**Current checkpoint (2026-08-09):** the user narrowed initial execution to Linux. Local/SSH terminal reconstruction, topology, forced network recovery, crash persistence, and lane isolation passed on remote Linux host through CUA and the Docker target. The actionable Linux notification was emitted, but its physical click and clipboard/IME checks remain blocked by the password-locked Wayland compositor. macOS is deferred, not waived. See `tests/integration/transport/README.md` for exact evidence.

### Phase 1 — Protocol, daemon, and connection foundation

1. Daemon/socket lifecycle and permissions.
2. Protocol handshake, requests, events, snapshots, cancellation, and sequence recovery.
3. Identical local and SSH bridge transports.
4. OpenSSH connection reuse and remote helper probe/install/upgrade.
5. Desktop connection reducer and persistence.

### Phase 2 — Complete tmux/terminal workspace

1. Control parser/dispatcher, per-session clients, flow control, and resnapshot.
2. Complete topology reducer and bidirectional session/window/pane actions.
3. Screen seed/snapshot lifecycle and hidden-pane resource management.
4. xterm input, selection, search, links, mouse, paste, and WebGL fallback.
5. Layout generation reconciliation and zoom.
6. Command registry, menus, palette, platform shortcuts.
7. Exact outer-server pane routing with fail-closed handling for unmatched hooks.

### Phase 3 — Application shell

1. Workspace rail, combined tab strip, split host, Explorer/Git switcher, and agent sidebar shell.
2. tmux focus/name/topology bindings.
3. App-owned tab restore.
4. Disconnected/read-only and helper-upgrade experiences.

### Phase 4 — Root, Explorer, editor, Markdown

1. Active-pane root discovery.
2. Lazy directories, watchers, overflow recovery, symlink policy, mutations.
3. Monaco open/save/external reload, content limits, binary/image handling.
4. Sanitized Markdown source/preview/split.
5. File/folder download UI and bulk engine integration.

### Phase 5 — Git

1. Repository identity/status parser and refresh scheduler.
2. Monaco staged/unstaged diffs.
3. File mutations.
4. Hunk patch operations and stale guards.
5. Commit and hook/error reporting.

### Phase 6 — Agents and notifications

1. Normalized adapter/state contracts and rollups.
2. Codex hooks/process/launch/resume/fallback.
3. Claude Code hooks/process/launch/resume/fallback.
4. Managed hook lifecycle.
5. Herdr-style sidebar/unseen/sounds.
6. Routed native notification activation and reconnect behavior.

### Phase 7 — Upload/paste and transfer hardening

1. Full preflight, progress, cancellation, digest, partial cleanup, concurrency.
2. Local file drop/copy and clipboard image staging/path paste.
3. Collision/large-upload confirmation.
4. 5 GiB upload/download and interactive-lane tests.

### Phase 8 — Hardening and internal release

1. Scale/fault/security/compatibility matrices.
2. Redacted diagnostics bundle.
3. Accessibility, IME, keyboard layouts, HiDPI, suspend/resume, notifications.
4. macOS universal and Linux x86-64/ARM64 packages; signing/notarization when credentials exist.
5. Setup, managed-file, hooks, uninstall, unsupported-feature scope, and destructive-action documentation.

## Verification

### Automated

- Rust unit/property/fuzz tests for control mode, protocol, reducers, paths, Git, agent transitions, and transfer accounting.
- TypeScript tests for snapshot/events, combined tabs, roots, editor reload, attention, progress, and stale-event rejection.
- tmux integration tests mutating topology through both protocol and external commands.
- SSH integration against disposable Linux hosts for install/upgrade/reconnect/latency/transfer/hooks/no-root behavior.
- Recorded terminal replay for shells, supported agents, alternate screen, mouse, OSC, Unicode, wraps, and resize.
- Git matrix for initial repos, worktrees, ignored/untracked, rename/delete, binary/conflict/hooks/stale hunks.
- Agent hook fixtures from supported Codex/Claude releases and Herdr-style fallback fixtures.
- Desktop E2E for UI flows plus focused native action tests where platform automation supports them.

### Manual release gates

1. Existing local tmux server attaches without recreation.
2. App and second normal tmux client remain synchronized through create/rename/split/resize/reorder/close.
3. App close/reopen preserves processes and restores app tabs.
4. Remote session recovers from forced disconnect without duplicate output/actions.
5. Codex/Claude locally/remotely produce correct working/blocked/idle/done/unseen states.
6. macOS/Linux notification click focuses exact pane after rename/reconnect; closed target degrades safely.
7. External/app file edits update Explorer/Markdown/Monaco/Git in realtime.
8. File/hunk Git operations and commit match CLI results.
9. Verified 5 GiB upload/download uses bounded memory and does not starve typing.
10. File/image paste into remote agents waits for verified staging and sends no Enter.

The former manual gate 11 is withdrawn. The feature is unsupported
and has no V1 physical release gate.

### Performance targets

- 20 sessions, 100 windows, 50 live panes.
- 250,000-file repository without eager recursion.
- 100 ms RTT.
- Topology: 250 ms local / 500 ms remote after receipt.
- Root: 500 ms local / 1 second remote.
- Explorer: 500 ms local / 1 second remote normally.
- Git: 1 second after normal change bursts settle.
- Fluid 60 Hz normal terminal output with bounded flood recovery.
- Hidden panes release GPU resources.
- Transfer memory remains bounded independently of content size.

## Principal risks and mitigations

- **Incomplete tmux screen replay:** release-blocking seed/replay gate, output buffering, renderer snapshots, scoped resnapshot.
- **Sizing oscillation with external clients:** visible-only size claim, tmux authority, generation reconciliation, no global option changes.
- **WKWebView/WebKitGTK differences:** test xterm/Monaco/IME/clipboard on both from Phase 0.
- **Output floods:** tmux pause-after, bounded queues, per-frame batching, hidden renderer disposal, capture recovery.
- **Large transfer starvation:** independent bulk TCP connections, strict concurrency, and the delayed/saturated Docker SSH regression gate.
- **Agent API evolution:** isolated adapters, versioned hooks (`MANAGED_VERSION` demotes a stale install to `partial`, which reinstalls), fixtures, and process detection for presence. Screen scraping was removed rather than maintained: it tracked vendor TUI output, so it broke on their release schedule.
- **Linux notification variation:** D-Bus capability detection and persistent in-app fallback.
- **Huge repositories:** lazy listing, excluded internals/dependencies, coalescing, watcher-overflow snapshots.
- **Reference licensing:** only reuse compatible code with attribution; cmux/Warp remain design-only absent a deliberate license decision.

## Reference material reviewed

The `./tmp` clones are research inputs, never runtime dependencies:

- cmux: `Sources/RemoteTmuxControlStreamParser.swift`, `Sources/RemoteTmuxControlConnection+Commands.swift`, `docs/remote-tmux-reconcile-design.md`.
- existing terminal tool: `src/renderer/src/components/terminal-pane/terminal-clipboard-paste.ts`, `src/main/window/clipboard-image-temp-file.ts`, xterm/Monaco dependency and renderer patterns.
- Herdr: agent manifests/detection, hook authority, state/unseen rollups, notifications, remote image bridge.
- Warp: remote daemon, versioned RPC, filesystem/Git patterns.

## Completion definition

Implementation is complete when the PRD release scenarios and the technical manual gates pass on macOS and Linux against local and remote tmux, with terminal replay and topology authoritative, Codex/Claude lifecycle matching the Herdr baseline, routed native notifications working where platform capabilities allow, and verified 5 GiB transfers remaining bounded and interactive.
