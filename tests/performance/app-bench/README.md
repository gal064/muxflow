# App bench: Tauri vs GPUI desktop

Measures the shipped Tauri app (`apps/desktop`: WebKitGTK/WKWebView, React,
xterm.js) against the GPUI port (`apps/desktop-gpui`) in real conditions: release
builds from one commit, a real compositor and GPU, the real `muxflow-host`
helper, a local tmux and a real SSH host. Both apps run one at a time, in the
same window geometry, against identically seeded scratch state.

| File | What |
|---|---|
| `run-linux.sh` | Linux entry point: checks tools and builds, runs `bench_linux.py`, `sizes.sh`, `summarize.py` |
| `bench_linux.py` | Linux driver (Wayland, Hyprland, amdgpu or any DRM driver with fdinfo) |
| `run-macos.sh`, `bench_macos.py` | macOS entry point and driver, same scenarios and result layout |
| `benchlib.py` | `/proc` CPU/memory/GPU accounting and window statistics, shared |
| `summarize.py` | Writes `summary.md`: median [min-max] per app and the Tauri/GPUI ratio |
| `sizes.sh` | Scenario 1: binary and package sizes |
| `workloads/` | Output generators, the typing probe, the remote sampler, the agent replay |

## Build (same commit for both)

```sh
cargo build --release --locked -p muxflow-gpui -p muxflow-host
(cd apps/desktop && pnpm install && ./node_modules/.bin/tsc -b && ./node_modules/.bin/vite build)
install -m 755 target/release/muxflow-host \
  apps/desktop/src-tauri/binaries/muxflow-host-$(rustc -vV | sed -n 's/^host: //p')
cargo build --release --locked -p muxflow      # Tauri; `custom-protocol` is a default feature
```

This is what `release/linux/build-package.sh` compiles (it adds a Debian-baseline
helper and a tarball). Both apps then find the same `target/release/muxflow-host`
beside them. On macOS build the `.app` with
`pnpm --dir apps/desktop tauri build --bundles app`, which stages the helper.

## Run

```sh
BENCH_SSH_CONFIG=/path/ssh_config BENCH_RECORDINGS=/path/recordings \
  tests/performance/app-bench/run-linux.sh /short/scratch/root [--runs 3 --hosts local,ssh ...]
```

The scratch root holds the apps' `HOME`, `ADE_HOST_RUNTIME_DIR` and
`TMUX_TMPDIR`; tmux and the helper put sockets there, so the path must stay
short (107 bytes for `.../t/tmux-UID/b`, 103 on macOS). The apps never see the
user's config, tmux server or helper daemon: the tmux server is isolated by
`TMUX_TMPDIR` plus `ADE_TMUX_SOCKET_NAME`, the helper by `ADE_HOST_RUNTIME_DIR`.
SSH reads the real `~/.ssh` through passwd, as it always does.

The SSH host needs only a user with tmux, perl and the bench key. The runner
installs the same `muxflow-host` build at `~/.local/bin/muxflow-host` (where
the app's installer puts it) when its checksum differs, copies the workloads to
`~/bench`, appends one line to `~/.bashrc` that sources `~/bench/bashrc`
(the magenta prompt), and runs `muxflow-host hook install` for the bench user.

### Compositor (Linux)

By default the runner starts a **private nested Hyprland** (same binary and
renderer as the session) with one headless 1920x1080@60 output, renders on the
real GPU, and puts the apps, `grim` captures and `wtype` keys on it. A window
there is visible and gets frame callbacks; it is not throttled. Nothing is
sent to the session compositor, and every `hyprctl` call is checked against
the session's instance signature. `BENCH_COMPOSITOR=session` uses the live
session instead; only do that on an unlocked desktop that is yours to drive,
because keys and window rules then go to that session.

## Method

**Process accounting.** Every sample walks `/proc`:

- `app`: the app's process tree (Tauri: `muxflow`, `WebKitWebProcess`,
  `WebKitNetworkProcess`, any GPU process, image-loader helpers), minus helpers.
- `host`: `muxflow-host` bridges in the tree, plus the daemon (found by its
  runtime dir) and its children.
- `tmux`: the bench tmux server and the helper's control-mode clients.
- `panes`: everything under the tmux server (the workload itself). Informational.
- `ssh`: the app's ssh clients and its control master.
- `compositor`: the Hyprland instance in use.

CPU is utime+stime from `/proc/<pid>/stat` as % of one core. A tracked
parent's cutime+cstime growth is credited (minus what its vanished tracked
children were already counted for), so short-lived children are not lost.
Memory is PSS and RSS from `smaps_rollup`, summed per category. GPU is the sum
of `drm-engine-*` busy ns from `/proc/<pid>/fdinfo`, deduplicated per
`drm-client-id`, as % of wall time (it can exceed 100 % with several engines).
On SSH runs `workloads/remote-sampler.pl` samples the remote user's
`muxflow-host`, `tmux`, `sshd` and pane processes the same way.

**Visible content.** Prompts and sentinels are drawn in colours the apps' chrome
never uses, and detected in compositor captures (`grim`, one frame per
capture, ~16 ms):

- the shell prompt has a magenta (#ff00ff) background (hue match, so a modal's
  dimming does not hide it);
- heavy output ends with a 40-cell #00ff00 bar, detected as a 150 px run;
- the typing probe answers each key with one #0000ff cell.

**Scenarios** (each in 3 runs, apps alternating who goes first):

1. Sizes: binary as built, stripped, gzip; package layout; linked system libs.
2. Startup: launch to window map (Hyprland `openwindow` event) and to the first
   frame showing the prompt. *Cold*: no app state, no helper daemon, a tmux
   session already present. *Warm*: seeded app state, daemon and tmux still
   running from the previous launch, binaries in page cache. (Cold is a fresh
   state, not a dropped page cache: that needs root.)
3. Idle, 1 workspace x 1 terminal: 60 s after a 15 s settle.
4. Idle, 8 tabs x 2 panes: 60 s.
5. Heavy output: `seq 1 3000000`, then `cat` of 200k colored lines (~25 MB),
   each followed by the sentinel. Wall time from send to sentinel on screen,
   CPU-seconds per category, peak PSS (0.5 s samples). Captures run every
   ~70 ms during this scenario and cost some compositor CPU, equally for both.
6. Agent-like stream: `workloads/stream.pl`, 60 colored, often wrapping lines/s
   for 60 s in one pane of the 8x2 layout.
7. Typing: `wtype` sends one key per process (0.25-0.4 s apart) to
   `workloads/keyprobe.pl`; latency is keystroke-sent to first capture showing
   the new cell. It includes wtype start-up and a keymap upload per key
   (identical for both apps) and is quantized to the ~16 ms capture interval.
8. Editor, keyboard-driven: Quick Open (`Ctrl+P`), type `big.txt` (5 MB),
   Enter; CPU and memory over the action (about 9 s). Screenshots of each step go to
   `<results>/<host>/evidence/`. Scrolling is not measured: PageDown reaches
   GPUI's editor but not Tauri's (no keyboard focus after open), and Wayland
   offers no pointer injection here. The Explorer action (`--scenarios
   explorer`: Show Files, Down, Right, 300 x Down) is kept but not run by
   default: in both apps the keys do not reach the file tree, so it measures
   nothing. A pointer-capable driver (ydotool, wlrctl, or the macOS meter's
   click) is needed for both.
9. Recorded agents: 6 panes (3x2 tiled, one tab) replay a real Claude Code and
   a real Codex session (3 each, staggered offsets) with
   `workloads/agent-replay.rs`, installed as `claude`/`codex` so the helper
   detects the agents. Rows are driven to working through the real hook path
   (`muxflow-host hook ingest`, fired with `tmux run-shell` so it runs under the
   tmux server like a real hook), one row goes done and back every ~10 s.
10. Spinner: the same 6 agents working, panes silent (`--idle` replays 5 s and
    stops), 60 s visible, then 60 s with the window on another workspace.

### Recording agent sessions

Recordings are not committed: they hold local paths and user names. Record on
a separate tmux socket, with the pane at the size the agent layout gets in the
app (`#{pane_width}x#{pane_height}` of a pane in the 3x2 layout), read-only:

```sh
tmux -S <scratch>/rec.sock -f /dev/null new-session -d -s claude -x 84 -y 17 \
  "cd <trusted repo> && unset TMUX TMUX_PANE && script -q -f --log-out claude.data \
   --log-timing claude.timing -c 'claude --permission-mode plan \"<read-only question>\"'"
```

Use `codex --sandbox read-only` for Codex. Use a directory both CLIs already
trust (answering a trust prompt writes to their settings). Unset `TMUX` so the
user's own agent hooks do not report into the user's real helper daemon.

## Results layout

`<results>/<host>/<scenario>/<app>-run<N>.json` (window summary),
`.ticks.jsonl` (raw samples) and `.remote.jsonl` (SSH host samples),
`evidence/*.png`, `logs/`, `meta.json`, `sizes.md` and `summary.md`.

## Known limits

- Typing latency is keystroke-to-capture through the compositor. It is not
  the app's internal input-to-paint. The perf-log probes need a
  `perf-log` build, which is not what ships.
- Each app's chrome differs, so the "same" pane is not the same cell grid:
  the 3x2 agent pane is 84x17 cells in GPUI and 84x18 in Tauri at 1600x1000.
- GPU busy % is engine time, not power. VRAM is reported when the driver
  exposes `drm-memory-vram`.
- macOS: see the docstring of `bench_macos.py` for what differs there.
