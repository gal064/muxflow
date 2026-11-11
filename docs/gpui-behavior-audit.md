# GPUI behavior audit (Phase 6, T6.3)

This audits the Phase 6 rows of the "Behavior audit" table in
[`gpui-migration.md`](gpui-migration.md) against the GPUI app in
`apps/desktop-gpui/src/**` and the Rust crates it shares with the host. Each
row names one statement from a user doc, whether the GPUI app does it, and
where.

Status values: **holds**, **differs**, **missing**, **exception** (an agreed
change in behavior), and **mac-only (unverified)**. A mac-only row was checked
only as far as the platform branch in the code; the owner verifies it on a Mac.

Result: none of the six docs names a removed technology, and none describes an
agreed exception, so **no user doc was edited**. The few statements that are
wrong for the GPUI app are listed under "Gaps for the root", and the edits that
cannot be made until Tauri is gone under "Cutover edits".

Host-side statements that no GPUI code touches (diagnostics field lists, staging-area and
file-safety notes, hook internals, the pi extension path, home-directory and locale
requirements) are covered by the grouped "host, unchanged" rows, not one row each.

`apps/desktop-gpui/src` is shortened to `gpui/` in the Where columns.

## docs/keybindings.md

| Statement | GPUI status | Where |
|---|---|---|
| The Linux keymap is the macOS keymap with ⌘ read as Ctrl, except Ctrl+D (EOF) and Ctrl+E (end of line), where Ghostty's default is used | holds | `gpui/commands/registry.rs` (`COMMANDS`, comments on `pane.splitRight` and `pane.zoom`); keymap test `linux_chords_resolve_with_ghostty_fallbacks` |
| Any command can be rebound or unbound in **Configure keyboard shortcuts**; commands without a default key are still in the palette | holds | `gpui/commands/shortcut_editor.rs`; `registry.rs` `palette_commands` hides only the 18 positional selectors, which all have defaults |
| Application, Linux: Ctrl+K, Ctrl+P, Ctrl+,, Ctrl+B, Ctrl+L, Ctrl+Shift+E, Ctrl+Shift+G, Ctrl+[, Ctrl+], Ctrl+Shift+U | holds | `gpui/commands/registry.rs`; `keymap.rs` test `every_default_resolves_from_the_keystroke_that_produces_it` |
| Application, macOS: ⌘K, ⌘P, ⌘,, ⌘B, ⌘L, ⌘⇧E, ⌘⇧G, ⌘[, ⌘], ⌘⇧U | mac-only (unverified) | `registry.rs` `Meta+…` defaults; `keymap.rs` `shortcut_from_keystroke` maps `platform` to `Meta`; `Platform::current()` |
| Quick Open matches files beneath the focused pane's directory, tabs in the workspace, and workspace names, paths and branches across shown hosts | holds | `gpui/commands/quick_open.rs` (`quick_open_items`); source in `gpui/workspaces/commands.rs` `Source` |
| Quick Open: Enter opens a file or switches to the tab or workspace; an open file appears as a tab result; file contents are not searched | holds | `gpui/commands/quick_open.rs`; file search is names and paths only (`"Searching filenames…"`) |
| An empty query lists tabs and workspaces at once; file search starts only after typing | holds | `quick_open.rs:177`, `:338` |
| A limited search asks you to narrow the query; workspaces and tabs stay available if file search fails | holds | `quick_open.rs:574-578` ("Search limited—narrow your query.", "File search unavailable. Workspaces and tabs are still searchable.") |
| The directory and host are shown below the results | holds | `quick_open.rs` footer (`render`, scope and footer labels) |
| Workspaces and tabs, Linux: Ctrl+N, Ctrl+1–9 (workspace), Ctrl+T, Ctrl+W, Ctrl+Shift+[ and ], Alt+1–9 (tab) | holds | `registry.rs`; keymap test `arrows_and_digits_use_their_stored_names` |
| Workspaces and tabs, macOS: ⌘N, ⌘1–9, ⌘T, ⌘W, ⌘⇧[ and ⌘⇧], ⌃1–9 | mac-only (unverified) | `registry.rs` (`tab.select*` use `Ctrl+N` on Mac, which is the Control key); GPUI reports ⌘⇧[ as `cmd-{`, handled by `keymap.rs` `unshifted` and tested |
| Positional selectors (⌘1–9, ⌃1–9) work while an editor has focus | holds | `keymap.rs` `is_positional_selector` |
| Panes, Linux: Ctrl+Shift+O (split right), Ctrl+Shift+D (split down), Ctrl+Alt+arrows (focus), Ctrl+Shift+arrows (resize), Ctrl+Shift+Enter (zoom) | holds | `registry.rs`; `pane.resize*` are `linux_keys` only |
| Panes, macOS: ⌘D, ⌘⇧D, ⌘⌥+arrow, no resize key, ⌘E | mac-only (unverified) | `registry.rs` (`pane.resize*` have no Mac key) |
| Terminal, Linux: Ctrl+C or Ctrl+Shift+C copy, Ctrl+V or Ctrl+Shift+V paste, Ctrl+F find | holds | `registry.rs` (`terminal.copy`, `.paste`, `.search`); `gpui/terminal/input/policy.rs` `terminal_clipboard_alias` |
| Terminal, macOS: ⌘C, ⌘V, ⌘F | mac-only (unverified) | `registry.rs` |
| Line start / end on macOS is ⌘← / ⌘→, absent on Linux | mac-only (unverified) | `gpui/terminal/input/policy.rs` `translate_terminal_key` (`platform != Mac` returns nothing; Ctrl-A/Ctrl-E on the normal screen, Home/End on the alternate screen) |
| Shift+Enter inserts a newline in an agent prompt, on both platforms | holds | `policy.rs` (sends `ESC [ 13 ; 2 u` for every program) |
| Linux Ctrl+C copies only while text is selected and clears the selection as it copies; with nothing selected it is the interrupt | holds | `keymap.rs` `shortcut_disposition` (`Yield`); `gpui/terminal/reading/mod.rs` (copy clears the selection on Linux; typed input drops it) |
| A selection left by Find does not count for Ctrl+C | holds | Find marks matches apart from the selection; the pane's selected flag follows only `term.selection` (`reading/mod.rs:865`, published at `:1077-1084`) |
| Ctrl+Shift+C and Ctrl+Shift+V always copy and paste | holds | `policy.rs` `terminal_clipboard_alias` (Linux only) |
| Muxflow takes Ctrl+W, Ctrl+V, Ctrl+B, Ctrl+K, Ctrl+L, Ctrl+P, Ctrl+N, Ctrl+T, Ctrl+F, Ctrl+[ on Linux, and unbinding returns them to the terminal | holds | `registry.rs`; `keymap.rs` `shortcut_for` honors a removed binding |
| On Omarchy, system-wide Super+C and Super+V also copy and paste | holds | Omarchy sends Ctrl+C or Ctrl+V to any window not tagged "terminal" (read in `omarchy/default/hypr/bindings/clipboard.lua`), which the app handles as above. A window tagged terminal would send Ctrl+Insert and Shift+Insert, which the terminal ignores (`gpui/terminal/input/keys.rs:66`). Not run on Omarchy |
| Shift-click pins or unpins a workspace, without switching to it | holds | `gpui/ui/chrome/sidebar.rs:148` |
| Shift-click pins or unpins a terminal tab, without selecting it | holds | `gpui/ui/chrome/tab_strip.rs:368` |
| Shift-click pins or unpins an agent's tab, without navigating | holds | `gpui/agents/view.rs:836` |
| Double-click renames a workspace | holds | `sidebar.rs:150` |
| Double-click renames a terminal tab | holds | `tab_strip.rs:381` |
| Double-click keeps a preview document tab open | holds | `tab_strip.rs:373-377` |
| Middle-click closes a tab | holds | `tab_strip.rs:418` (mouse-up) |
| Right-click opens the menu of a workspace, tab, agent, file or change | holds | `gpui/context_menu.rs`; `sidebar.rs`, `tab_strip.rs`, `agents/view.rs` |
| The Menu key or Shift+F10 opens the same menu from the keyboard | missing | `gpui/context_menu.rs:9` says it is not carried over: Kit opens a context menu only on a right or long press |
| ⌘-click or Ctrl-click opens a terminal link; elsewhere in the pane the same press grabs the pane | holds | `gpui/terminal/reading/links.rs` `is_link_activation`; `gpui/terminal/surface.rs` `press_frame` |
| Right-click in the terminal selects the word under the pointer | holds | `gpui/terminal/reading/mod.rs` `press_right` (skipped when the press is inside an existing selection) |
| Shift-drag selects even when the program uses the mouse; Option-drag does too on macOS | holds (Linux); mac-only (unverified) for Option | `gpui/terminal/input/mod.rs:428` `forces_local_selection` |
| Drag on the title bar moves the window, on Linux and on the empty part of the bar on macOS | holds (Linux); mac-only (unverified) | `gpui/ui/title_bar.rs` (`drag_tracker`; double-click zooms on Linux, uses the system action on macOS) |
| Drag a tab to reorder; drop on a workspace to move it; a single-pane terminal tab dropped on a pane edge becomes a split | holds | `gpui/dragdrop/rules.rs` `plan_drop`, `ONE_PANE_TAB_ONLY`; `gpui/ui/chrome/tab_strip.rs` drop gaps |
| Drag an agent onto a pane edge (split), the tab strip (own tab) or a workspace (move) | holds | `gpui/agents/view.rs` `agent_drag_payload`; `dragdrop/rules.rs` |
| ⌘-drag or Ctrl-drag a pane to move it (another pane's edge, the tab strip, a workspace) | holds | `gpui/terminal/surface.rs` `press_frame` |
| Escape cancels a drag | holds | `gpui/dragdrop/mod.rs:247`; test `drops_nothing_when_escape_cancels_the_drag_and_swallows_the_release` |
| Moving a tab, agent or pane keeps the screen where it is, unless it is the pane in view; a move that would empty a workspace is refused | holds | `gpui/dragdrop/rules.rs` (a move must not empty the source workspace) |
| On Linux the Ctrl-press that grabs a pane is not passed to the terminal, but on a link it opens the link | holds | `gpui/terminal/input/mod.rs` `claims_press` excludes the secondary left press; `surface.rs` `press_frame` skips grabbing on a link. A frozen (read-only) pane does not grab, but the press still is not passed on |
| **Show pinned workspaces only** is in the command palette | holds | `registry.rs` `workspaces.showPinnedOnly` |
| Agent menu has **Bring here as split** and **Move to workspace…** | holds | `gpui/agents/view.rs:567`, `:596` |
| **Focus pane on hover** focuses a split pane when the pointer rests on it | holds | `gpui/settings/dialog.rs:685`; `gpui/terminal/surface.rs:138`, `:822` |
| **Copy on select** copies as soon as the selection is finished | holds | `gpui/settings/dialog.rs:652`; `gpui/terminal/reading/mod.rs:714` |
| ↑ / ↓, Enter in the command palette and Quick Open | holds | `gpui/commands/palette.rs`, `quick_open.rs` (`MoveUp`/`MoveDown` actions) |
| ← / →, Home / End on a focused tab strip | holds | `gpui/workspaces/mod.rs:2064` `moved_cursor`; `tab_strip.rs:723-763` |
| ↑ / ↓ on a focused workspace list | holds | `sidebar.rs:780-815` |
| ↑ / ↓ on a focused agent list | missing | `gpui/agents/view.rs` has no key handler and no list focus handle; rows are buttons reached with Tab |
| ↑ / ↓ and Escape in an open menu | holds | Kit `PopupMenu` binds up, down and escape (`gpui-component` `menu/popup_menu.rs:25-27`); `gpui/context_menu.rs` |
| Home / End and Tab in an open menu | differs | Kit binds only enter, escape and the four arrows (`popup_menu.rs:24-29`); `context_menu.rs:5` claims Home and End, which no binding provides |
| Escape cancels a dialog | holds | Kit dialogs; `gpui/commands/dialogs.rs` `escape_ends_composition` |
| Enter in the find bar goes to the next match | holds | `gpui/terminal/reading/mod.rs:757` (Shift+Enter goes back) |
| ⌘Enter or Ctrl+Enter in the commit message commits | holds | `gpui/git/panel.rs:1216-1224` (`Enter` action with `secondary`) |
| ← / → on the focused sidebar edge resizes the sidebar | holds | `gpui/ui/chrome/divider.rs` `key_delta` |
| ↑ / ↓ on the focused divider above the agent list resizes the agent list | holds | `divider.rs` `Divider::Agents` |

## docs/remote-host.md

SSH, probing and installation run in `crates/desktop-core/src/connection/**`
and `apps/host`, which the GPUI app shares unchanged with the host. Only the
confirmation and recovery UI is GPUI code.

| Statement | GPUI status | Where |
|---|---|---|
| Remote connections use the system OpenSSH client, which keeps authentication, known hosts, agent forwarding, `ProxyJump` and key choice | holds | `crates/desktop-core/src/connection/transport.rs`, `transport/control_master.rs` |
| A private temporary control socket; bulk transfers use separate non-multiplexed connections | holds | `control_master/socket.rs:85` (mode 0700); `transport.rs:141-170` (`ControlMaster=no`, `ControlPath=none`) |
| No public or TCP listener is opened | holds | no `TcpListener` in `apps/desktop-gpui` or `desktop-core`; the host binds only a Unix socket (`apps/host/src/daemon.rs:57`) |
| The helper is installed at `~/.local/bin/muxflow-host` only after explicit confirmation | holds | `gpui/connection_ui/dialog.rs:105-150` ("Install remote helper?", "Install and connect"); `gpui/connection_ui/helper_upgrade.rs` |
| Install probes OS and architecture, picks the matching helper, uploads to a private temporary path, checks SHA-256, OS and architecture, atomically replaces, and restores the previous helper if the new handshake fails | holds | `desktop-core/src/connection/helper.rs:124-186` (`helper_artifact`, `HelperRollback`); `apps/host/src/remote_helper.rs` |
| An incompatible helper makes the UI read-only until an explicit upgrade succeeds | holds | `gpui/connection_ui/helper_recovery.rs`; `connection_reducer.rs:119` (`can_mutate` only while Connected) |
| Mutations created before a disconnect are never replayed | holds | `gpui/workspaces/mod.rs` (`SCOPE_CHANGED` is dropped, not retried); `connection_reducer.rs` |
| Hosts are Linux x86-64 or ARM64, or macOS on Apple Silicon; Intel Macs are not supported | holds | `desktop-core/src/connection/helper.rs:230-238` (error "Intel Macs aren't supported as SSH hosts") |
| tmux 3.3 or newer; on a Mac, Homebrew and MacPorts tmux are found without PATH | holds | `apps/host/src/remote_helper.rs:171`, `:207`; `crates/tmux-control/src/executable.rs:142-145` |
| Git is needed only for Git features | holds | probe reports Git separately (`RemoteHelperProbe.git_version`) |
| Mac host: turn on Remote Login; Tailscale's own SSH server does not run in the Tailscale Mac app; allow full disk access for remote users | mac-only (unverified) | a setup step on the Mac, not app code |
| The daemon socket and runtime files are user-only; a daemon restart does not restart tmux | holds | `apps/host/src/daemon.rs:83` (metadata 0600), `paths.rs:32` (`/tmp/muxflow-<uid>`); tmux is its own server, and the GPUI app never starts or stops it with the daemon (not exercised) |
| If setup fails, run `doctor` on that host and generate a support bundle | holds | `apps/host/src/main.rs:109-110` |
| Unicode session and window names need a UTF-8 locale; renames are bidirectional and authoritative after reconnect; pane titles stay with tmux | holds | host and `tmux-control`; GPUI renames go through `session.rename` and `window.rename` (`gpui/workspaces/commands.rs`) |

## docs/mac-ssh-hosts.md

This is a dated plan and test record, not user-facing copy. Every statement
about the desktop side (probing, helper choice, port detection, `ControlPersist`)
lives in `crates/desktop-core`, which the GPUI app uses as is. The
test-result sections describe past runs and are not re-asserted. A machine name
and user name in the "Test Mac access and cleanup" section were not copied
here.

| Statement | GPUI status | Where |
|---|---|---|
| The probe reports OS and architecture; the artifact is picked by both, never architecture alone | holds | `desktop-core/src/connection/helper.rs:186-250` |
| An Intel Mac gets "Intel Macs aren't supported as SSH hosts" | holds | `helper.rs:236`, `:275` |
| Packaged names are `muxflow-host-{linux,macos}-{arch}`, beside the executable or in `Contents/Resources`; a same-OS, same-architecture host reuses the app's own helper | holds | `helper.rs:200-218` |
| Test overrides are `ADE_HOST_HELPER_{LINUX,MACOS}_{X86_64,AARCH64}_PATH` | holds | `helper.rs:193-198` |
| Install refuses a wrong OS, wrong architecture and tmux older than 3.3; reads ELF and Mach-O headers | holds | `apps/host/src/remote_helper.rs` (`executable_target`, `tmux_supported`, tests at `:847`) |
| tmux candidates include Homebrew, MacPorts and pkgsrc, after the Linux ones | holds | `remote_helper.rs:171`; `crates/tmux-control/src/executable.rs:142-145` |
| Port detection uses `ss`, then `lsof -F` where `ss` is missing, and keeps ports 1024 and up on loopback or wildcard | holds | `desktop-core/src/connection/ports.rs:359-380`; test `detected_ports_on_a_mac_come_from_lsof` |
| Both control-master spawns pass `-o ControlPersist=no` | holds | `control_master.rs:618-637` |
| Killing the app leaves the control master running (a known limit, Todo) | holds | unchanged; no GPUI change touches `control_master` |
| Same-digest coexistence: two machines on different releases retire each other's daemon | holds | `apps/host` `daemon_is_current_build`; unchanged |
| Agent-status hooks are shared between a Mac's app and a remote install (known conflict, Todo) | holds | host-side, unchanged |
| macOS-only behaviors to verify (TCC, daemon survives logout, Gatekeeper, git shim) | mac-only (unverified) | not app code; the doc already marks them as to verify |
| Code references use Tauri-era paths such as `connection/helper.rs` and `connection/ports.rs` | differs | now `crates/desktop-core/src/connection/{helper,ports}.rs`. A history-style plan, so see "Cutover edits" |

## docs/agent-hooks.md

Hook installation, ingest, state decay, Codex and Claude handling and the CLI
(`muxflow-host hook …`) are host-side (`apps/host/src/service/agents/**`,
`apps/host/src/hooks`), which the port did not touch (the port's host changes are terminal-engine fixes only). Those rows say "host,
unchanged" and were spot-checked, not re-derived. The rows
below are the desktop-facing ones.

| Statement | GPUI status | Where |
|---|---|---|
| An agent without hooks appears in the list and reads `unknown` | holds | `gpui/agents/model.rs` (`DisplayState::Unknown`, rank 2) |
| Hook installation is a separate, reviewable action from helper installation | holds | `gpui/agents/setup.rs`; `gpui/connection_ui/dialog.rs:124` (helper dialog) vs `agents/view.rs` review dialog |
| The review shows every touched file, before and after hashes, a redacted diff, the exact managed command and adapter trust guidance; confirming creates a backup | holds | `gpui/agents/view.rs:1447-1494`, `:1587`; `gpui/agents/wire.rs:331-389` (`backup_path`) |
| Wiring states `wired`, `partial`, `notWired`, `absent`, `unavailable`, `unspecified` | holds | `gpui/agents/wiring.rs`; `gpui/agents/model.rs` (`HookWiring`) |
| On connect the desktop asks once per host profile, only when the host reports nothing; the answer is remembered either way | holds | `gpui/agents/wiring.rs:74` `should_prompt_for_setup`; `gpui/agents/setup.rs` (`HostSetupDecision`) |
| **Settings → Connection → Set up agent status…** and the agents section's menu revisit the choice | holds | `gpui/settings/dialog.rs:545`; `gpui/agents/view.rs:623` ("Set up agent status on this host…") |
| An already-consented host is brought current on connect without asking again | holds | `gpui/agents/setup.rs` (re-run on every change of hosts and snapshots) |
| Removing hooks records a decline, so they are not added back | holds | `gpui/agents/setup.rs:586` (`HookAction::Uninstall` becomes `Declined`) |
| The agents menu offers Install and Uninstall per adapter that supports hooks | holds | `gpui/agents/view.rs:648-676` |
| Every write is bound to the host that was answered about | holds | `gpui/agents/setup.rs:1-12` (identity captured with the question, checked before each request) |
| Codex 0.157+ empty `CODEX_EXEC_SERVER_URL` and `daemon_auto_start = false` handling on connect | holds | host-side: `apps/host/src/service/agents/codex_config.rs`, `tmux_config.rs`; unchanged |
| `muxflow-host hook status`, `install`, `uninstall` and `--yes`, `--home`, `--settings-path` | holds | host-side CLI, unchanged |
| pi extension file, markers, event mapping and `MUXFLOW_PI_OWNER` | holds | host-side, unchanged |
| Events taken, pane ownership, decay (15 minutes, 24 hours), Claude `sessions/<pid>.json`, caps and limits | holds | host-side (`apps/host/src/service/agents/**`), unchanged |
| Hooks submit compact state JSON through the private daemon socket and never send prompt text, output or credentials | holds | host-side, unchanged; the GPUI app reads only the daemon's snapshots |
| `quiet_wakeup_held` is recorded in diagnostics | holds | host-side |
| Agent state and attention in the app (working, blocked, done, idle, unknown; jump to the agent that needs you) | holds | `gpui/agents/model.rs` (ordering "blocked, working, done, unknown, idle"); `registry.rs` `agents.jumpUnread` |

## docs/destructive-actions.md

| Statement | GPUI status | Where |
|---|---|---|
| Confirmation before killing a tmux session | holds | `gpui/workspaces/commands.rs:918-990` (`session.close` asks, "Close permanently") |
| Confirmation before deleting a non-empty directory | holds | `gpui/files/dialog.rs:268` ("Delete {path}?" with the non-empty detail) |
| Confirmation before overwriting a file | holds | `gpui/files/dialog.rs:311-326` ("Allow overwrite after confirmation", "Also replace a non-empty destination directory") |
| Confirmation before discarding Git changes | holds | `gpui/git/panel.rs:112-345` (`PendingDiscard`, `prepare_discard`) |
| Confirmation before pasting an upload larger than 500 MiB | holds | `gpui/terminal/integration/transfers.rs:15` (`LARGE_UPLOAD_BYTES`); `upload.rs:285`, `:526` |
| The confirmation names the exact target and is revalidated against the current server and root generation | holds | `gpui/git/panel.rs:291-292` (`expected_status_generation`, `expected_source_generation`); `gpui/agents/setup.rs`; host-side checks |
| Closing a terminal tab or pane from the tab strip has no dialog | holds | `gpui/workspaces/commands.rs:918-921`; `registry.rs` marks `window.close` and `pane.close` destructive, but `commands.rs` closes them on the spot |
| Bulk closes (Close Others, Close Tabs to the Right, Close All Non-Agent Tabs and toolbar buttons) have no dialog | holds | `gpui/ui/chrome/tab_strip.rs:609`, `:891-893` |
| The wire contract still carries `confirmed` and the precondition | holds | `gpui/workspaces/commands.rs:534`, `:972` (`confirmed: true`) |
| The phone asks for no confirmation | holds | mobile app, not in scope here |
| Dirty editors are flushed before a bulk close, and the set is abandoned if a save fails | holds | `gpui/workspaces/commands.rs:476-500`, `:700-703` (`editor::flush_all`) |
| A terminal that gained an agent is held back at the mutation boundary; the outcome is a counted receipt or survivor list; a survivor keeps document tabs open | holds | `gpui/workspaces/model.rs:306-325` ("Closed {n} of {m} …; {k} still had an agent and … left open") |
| Stale mutations are rejected after reconnect; file and Git requests are scoped to a root identity; traversal, symlink traversal, stale hunks and destination replacement are rejected | holds | host and `desktop-core` (`files/destination_lease.rs`, `local_staging.rs`); GPUI passes the generation (`git/panel.rs:291`) |
| Cancelled or failed transfers publish nothing complete-looking; byte count and BLAKE3 verified; only owned partials cleaned | holds | `desktop-core/src/connection/files/**`; `gpui/transfers.rs` |

## docs/diagnostics-privacy-security.md

Removing the webview changes none of the promises below. The app has no
embedded web content, so the Tauri content-security and IPC surfaces are gone,
not weakened. The only network use in the GPUI app is the update check and the
installer it starts, through `desktop-core`'s `ureq` and `curl`.

| Statement | GPUI status | Where |
|---|---|---|
| `muxflow-host doctor`, `doctor --json` and `support-bundle --output` exist | holds | `apps/host/src/main.rs:109-110`; `apps/host/src/diagnostics.rs` |
| The support bundle is local-only, never uploaded, with no fields for terminal output, prompts, file contents, notifications, environment values, SSH config, credentials, usernames, hostnames or paths | holds | `apps/host/src/diagnostics.rs` (strict schema); nothing in `apps/desktop-gpui` uploads |
| Bundles are new user-only (0600) files; existing files and symlink destinations are refused | holds | `apps/host/src/diagnostics.rs:809-820` |
| The daemon listens only on its Unix socket; diagnostics add no TCP listener | holds | `apps/host/src/daemon.rs:57`; no listener in `apps/desktop-gpui` or `desktop-core` |
| Authentication and host-key verification stay with OpenSSH; the app does not copy or store keys or passwords | holds | `desktop-core/src/connection/transport.rs` (`BatchMode=yes`); `profiles.json` holds targets and config paths only |
| `/tmp/muxflow-<uid>` and the durable state directory are mode 0700 | holds | `apps/host/src/paths.rs:32`; `desktop-core/src/app_state.rs:409`, `:806`; `profiles.rs:107` |
| No telemetry is sent | holds | no HTTP client in `apps/desktop-gpui`; the only outbound call is `update_check` |
| The update check fetches `…/releases/latest/download/latest.json` at launch and then at most once a day | holds | `desktop-core/src/update_check.rs:21`; `gpui/update.rs:17` (`INTERVAL` is 24 hours) |
| It shows a red "Update" pill when a newer release is published | holds | `gpui/ui/chrome/title_content.rs:194-210` (`.danger()` button, "↑ Update {version}") |
| Clicking the pill on the desktop runs the published installer | holds | `desktop-core/src/update_check.rs:30`, `:62-80`; refused outside the default install location |
| Tapping the pill on Android downloads the APK | holds | mobile, out of scope |
| Remote images and scripts in rendered content cannot phone home | holds | `gpui/markdown/view.rs` `embedded_image` allows only inline raster data URIs, tested against `https:`, `file:` and SVG sources. This replaces what the webview's content-security policy did |
| The doc says nothing of the always-on incident journal (`incidents.jsonl`, 2 MiB, one rotated file) the app writes to its log directory | differs | `desktop-core/src/incidents.rs`; `gpui/diagnostics.rs`; `gpui/paths.rs:27-35`. It holds ids, timings and reasons, and `action.refused` keeps up to 200 characters of a host error. Its file mode follows the umask, not 0600 |

## Gaps for the root

Every "differs" or "missing" row above, with a suggested owner.

1. **Menu key and Shift+F10 do not open a context menu.** Documented in `keybindings.md`; `gpui/context_menu.rs:9` records it as not carried over because Kit opens a menu only on a right or long press. Owner: T6.1 `accessibility`.
2. **Open menu: Home, End and Tab are not bound.** `keybindings.md` lists them; Kit's `PopupMenu` binds only enter, escape and the arrows, and the comment in `context_menu.rs:5` wrongly says Home and End come from Kit. Owner: T6.1 `accessibility`. If it is not fixed, drop "Home / End, Tab" from the doc at cutover.
3. **Focused agent list has no ↑ / ↓ row navigation.** The workspace list and tab strip have it (`moved_cursor`); `gpui/agents/view.rs` has no key handler or focus handle on the list. Owner: T6.1 `accessibility`.
4. **The incident journal is undocumented and not user-only.** `diagnostics-privacy-security.md` does not mention `incidents.jsonl`, and the file and its log directory take the process umask instead of 0600 and 0700 (`desktop-core/src/incidents.rs`, `gpui/paths.rs`). `action.refused` may carry up to 200 characters of a host error, which could include a path. The TypeScript journal (`apps/desktop/src/diagnostics/incidents.ts`) fed the same `desktop-core` writer, so this is not a webview regression. Owner: the root, to decide between a doc paragraph and tightening the file mode (the latter is a code change, so T6.7 if taken).
5. **`mac-ssh-hosts.md` cites Tauri-era source paths** (`connection/helper.rs`, `connection/ports.rs`). The code now lives in `crates/desktop-core`. Owner: the root (doc edit at cutover, see below).

## Cutover edits

Doc changes to make when Tauri is removed. None can be made now, because the
Tauri release is still the shipping app.

- `docs/mac-ssh-hosts.md`: replace the desktop source references (`connection/helper.rs`, `connection/ports.rs`, "desktop") with `crates/desktop-core/src/connection/{helper,ports}.rs`. Better: move the whole plan to `docs/history/` once the Mac host work is done, since it is dated and describes past test runs. Keep the machine name and user name out of any copy.
- `docs/keybindings.md`: if gaps 1 or 2 are not fixed, remove the Menu key and Shift+F10 sentence from the Mouse table and "Home / End, Tab" from the open-menu row.
- `docs/diagnostics-privacy-security.md`: add one sentence that the desktop also keeps a local incident journal (ids, timings, reasons; never uploaded), once gap 4 is decided.
- `docs/keybindings.md`: the Omarchy Super+C and Super+V sentence is true only while Omarchy does not tag the app as a terminal. No edit now; if the owner finds it false on Omarchy, remove the sentence.

## Mac prompt items

What the owner should check on the Mac.

1. Every macOS column chord in `keybindings.md` runs its command: ⌘K, ⌘P, ⌘,, ⌘B, ⌘L, ⌘⇧E, ⌘⇧G, ⌘[, ⌘], ⌘⇧U, ⌘N, ⌘1–9, ⌘T, ⌘W, ⌘⇧[, ⌘⇧], ⌃1–9, ⌘D, ⌘⇧D, ⌘⌥+arrows, ⌘E, ⌘C, ⌘V, ⌘F. Confirm ⌘⇧[ and ⌘⇧] switch tabs (GPUI reports the shifted character) and that ⌃1–9 selects tabs while ⌘1–9 selects workspaces.
2. ⌘← and ⌘→ in a shell prompt move to line start and end (Control-A and Control-E), and in a full-screen program such as an agent composer send Home and End.
3. ⌘C copies a selection and, with nothing selected in an agent composer, does nothing harmful; ⌘V pastes. ⌃C always interrupts.
4. Option-drag selects text while a mouse-reporting program (vim, tmux mouse mode) is running; Shift-drag does too.
5. ⌘-click opens a terminal link; ⌘-click on blank pane area grabs the pane for a drag; ⌘-drag moves the pane to another pane's edge, the tab strip or a workspace.
6. Title bar: dragging the empty part moves the window and double-clicking it zooms or minimizes per the system setting. Dragging a button area does not move it.
7. Application menu keys: ⌘Q, ⌘H, ⌥⌘H, ⌘M and ⌃⌘F work, and a menu item shows the same shortcut the keymap uses (including after a rebind in Configure keyboard shortcuts).
8. Mac as an SSH host: Remote Login on, the helper installs and the daemon outlives the SSH session, Homebrew tmux is found, Files and Git work in Documents with full disk access for remote users on, and Intel Macs get the refusal message.
9. Update pill: with a newer release published, the red pill appears at launch, and clicking it runs the installer and relaunches.
