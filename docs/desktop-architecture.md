# Desktop architecture

The desktop app is a native Rust app built on GPUI, through GPUI Kit
(`gpui-kit`). It talks to the same `muxflow-host` helper and wire protocol as
the mobile app.

## Crates

| Crate | What lives there |
|---|---|
| `apps/desktop` (`muxflow`) | The GPUI app: windows, views, commands, settings. Its module map is in `src/lib.rs` |
| `crates/desktop-core` | Everything that is not UI: connections and transport, transfers, profiles, app state, notifications, incidents, the update check and the perf-log sink. No GPUI |
| `crates/terminal-engine` | One `alacritty_terminal` per pane, and the event path around it: generations, seeds, backlog, pane health, history paging. No GPUI, so it runs in plain unit tests |
| `crates/terminal-interactions` | Links, file paths and copy cleanup in terminal text. The mobile app's `packages/terminal-interactions` runs the same fixtures |
| `crates/live-markdown` | Live Markdown editing; only its `view` module uses GPUI |
| `crates/protocol`, `crates/tmux-control`, `apps/host` | The wire protocol, tmux control mode and the host helper, shared with mobile |

Colors come from `packages/client-core/src/palette.ts`, which generates both the
mobile CSS and `apps/desktop/src/theme/palette.rs`
(`pnpm --filter @muxflow/client-core palette:gen`).

## Threads

- **GPUI's main thread** runs every view and entity.
- **`desktop-core`'s tokio runtime** runs connections, SSH, transfers and file
  work. Results come back to the UI over channels.
- **One `terminal-engine` thread per shown host** parses terminal output into
  each pane's grid.

No thread outside GPUI wakes a GPUI task. The UI drains each channel on a GPUI
timer instead (`apps/desktop/src/terminal/link.rs`): every 4 ms while output or
input is moving, once a frame while idle, and slower while the window is hidden.
GPUI's test scheduler rejects foreign wakeups, so this keeps every path testable.

## Terminal data path

```text
host ─► desktop-core bridge ─► terminal-engine thread ─► grid + damage
                                     │                       │
                         host credit ack              drained on a timer
                                                             ▼
                                               TerminalElement paints rows
```

Bytes are never serialized between threads. GPUI repaints at most once a frame,
however much output arrived. Hidden panes keep their grids in memory.

## Flow control

The host sends terminal output against a credit window
(`crates/desktop-core/src/connection/delivery_window.rs`). The engine acknowledges a
frame only after it has applied it, and `delivery_ack.rs` turns those
acknowledgements back into host credit. The engine's inbox is unbounded on
purpose: the window is the bound.

## Updating GPUI

`gpui-kit` is pinned to an exact version, and it pins its matching `gpui-pre`.
Bump it in its own pull request:

1. Change the version in `apps/desktop/Cargo.toml` (both the dependency and the
   dev-dependency) and run `cargo update -p gpui-kit`.
2. Check each `vendor/*/VENDORED.md`: a vendored crate is removed once upstream
   has the fix it carries.
3. Run `cargo deny check licenses`, the workspace tests (the `frames` test
   renders real frames, so it needs a Vulkan driver) and the macOS CI job.

## Licenses

The desktop app is MIT. No GPL or AGPL code may enter it. `cargo deny check
licenses` gates CI against the allowlist in `deny.toml`. Do not read, port or
paraphrase Zed's `editor`, `terminal`, `terminal_view` or `workspace` crates;
work from the public APIs of `gpui`, `gpui-kit` and `alacritty_terminal`.
