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
| `crates/protocol`, `crates/tmux-control`, `crates/phase0-core`, `apps/host` | The wire protocol, tmux control mode, notification routing and the host helper |

Colors come from `packages/client-core/src/palette.ts`, which mobile imports
directly. `pnpm --filter @muxflow/client-core palette:gen` writes
`apps/desktop/src/theme/palette.rs` and `packages/client-core/palette.css` from it.

## Threads

- **GPUI's main thread** runs every view and entity.
- **`desktop-core`'s tokio runtime** runs connections, SSH, transfers and file
  work. Results and bridge events come back to the UI over async channels that
  GPUI tasks await.
- **One `terminal-engine` thread per shown host** parses terminal output into
  each pane's grid.

The engine thread wakes no GPUI task. The UI drains its output on a GPUI timer
(`apps/desktop/src/terminal/link.rs`): every 4 ms while output or input is
moving, once a frame while idle, and slower while the window is hidden. GPUI's
test scheduler rejects wakeups from a plain thread, so this keeps the terminal
path testable.

## Terminal data path

```text
host ─► desktop-core bridge ─► connection controller ─► terminal-engine thread
           (tokio)               (GPUI main thread)            │
              ▲                                          grid + damage
              │                                                │
       host credit ◄──── frame acknowledged ◄──── drained on a timer
                                                               ▼
                                                 TerminalElement paints rows
```

Bytes are never serialized between threads. GPUI repaints at most once a frame,
however much output arrived. Hidden panes keep their grids in memory, up to a
bound (64 screens or 128 MiB, `crates/terminal-engine/src/engine.rs`).

## Flow control

`desktop-core` hands terminal frames to the UI only inside a delivery window
(`crates/desktop-core/src/connection/delivery_window.rs`, 2 MiB or 4,096 frames).
The engine acknowledges a frame after it has applied it, and `delivery_ack.rs`
turns those acknowledgements into the host's terminal credit. The engine's
inbox is unbounded on purpose: the window is the bound.

## Updating GPUI

`gpui-kit` is pinned to an exact version, and it pins its matching `gpui-pre`.
Bump it in its own pull request:

1. Change the version in `apps/desktop/Cargo.toml` and
   `crates/live-markdown/Cargo.toml` (each has it as a dependency and a
   dev-dependency) and run `cargo update -p gpui-kit`.
2. Check each `vendor/*/VENDORED.md`. `vendor/gpui-pre-macos` must be vendored
   again at the new `gpui-pre` version, or the `[patch]` stops applying; a
   vendored crate is removed once upstream has the fix it carries.
3. Run `cargo deny check licenses`, the workspace tests (the `frames` test
   renders real frames, so it needs a Vulkan driver) and the macOS CI job.

## Licenses

The desktop app is MIT. No GPL or AGPL code may enter it. `cargo deny check
licenses` gates CI against the allowlist in `deny.toml`. Do not read, port or
paraphrase Zed's `editor`, `terminal`, `terminal_view` or `workspace` crates;
work from the public APIs of `gpui`, `gpui-kit` and `alacritty_terminal`.
