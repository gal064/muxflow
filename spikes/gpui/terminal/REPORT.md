# T0.4 spike-terminal: can a custom GPUI element fed by Muxflow's real tmux parser match xterm?

Answers S1, S2 and the terminal half of S5 in `docs/gpui-migration.md`, with numbers.

**Recommendation: continue with this architecture to the T0.7 comparison and a real-GPU
rerun. Nothing here rules it out, and nothing here proves it: S1 and S2 are not passed.**
At rest and in typing into a pane that is not itself flooded, the renderer's own costs fit
inside a frame, frames coalesce, and memory stayed bounded. But the pass criteria are
"no worse than xterm", this report contains no xterm number (T0.7 captures the Tauri
baseline separately), the display is software-rendered, and interaction under flood takes
seconds. Three problems found here are
open and must be addressed by the real implementation; see "Defects found" and "What would
change this".

All numbers are from a local run (Xvfb, lavapipe/llvmpipe, 16 CPUs, tmux 3.8)
produced by `measure.sh`, except where a section says otherwise. Load average at the start
of each scenario is in `load.txt`; the script waits (bounded) for load below 6 before each
scenario.

## Results

Latency is ms, p50 / p95 / p99.

### S1: keystroke latency

| Scenario | n | timeouts | leg (a) key to next `%output` (echoLagProbe) | leg (b) key to frame present returned | echo applied to frame build start (tick wait, inside leg b) | main-thread wait before t0 |
|---|---|---|---|---|---|---|
| 1 pane, idle | 600 | 0 | 0.36 / 0.47 / 0.54 | 11.21 / 19.51 / 19.87 | 7.11 / 15.52 / 15.80 | 0.17 / 1.02 / 1.27 |
| 8 panes, near empty | 600 | 0 | 0.38 / 0.50 / 0.57 | 10.87 / 18.73 / 18.99 | 7.68 / 15.56 / 15.89 | 0.17 / 1.04 / 1.32 |
| 8 panes, filled with text | 600 | 0 | 0.38 / 0.50 / 0.59 | 14.96 / 21.56 / 22.29 | 8.30 / 14.89 / 15.45 | 0.16 / 0.97 / 1.33 |
| 8 panes, 7 flooding, typing in the quiet pane | 1308 | 0 | 2.98 / 4.15 / 15.12 | 13.45 / 25.05 / 29.42 | 2.84 / 14.26 / 14.82 | 2.38 / 8.80 / 10.05 |

The "near empty" row is shell prompts only. Its present time is 2.55 ms against 5.66 ms with
the panes filled, so the filled row is the one to read for a realistic 8-pane workspace.

Setup behind every row: window 1280x800 at scale 1, about 163x44 cells; in the 8-pane layout
each pane is about 52x14. Build cost scales with visible rows and software present with pixels.

Flood row: the main-thread wait is real delay before the key is handled, and it comes before
t0, so leg (b) understates it. **From the driver's deadline to frame present, per sample, is
19.24 / 27.11 / 34.62 ms.** That wait also phase-locks keys to the tick (tick wait p50 is
2.84 ms under flood against 7-8 ms idle), so this row is not the same kind of number as the idle rows.

Leg (b) at frame paint (before the renderer runs): 7.59 / 15.94 / 16.24 for 1 pane,
8.17 / 16.05 / 16.37 for 8 near-empty panes, 8.76 / 15.40 / 16.00 for 8 filled panes,
7.03 / 18.58 / 22.75 under flood.

Three repeats of 1 pane and 8 panes, with different random seeds, agree within 0.6 ms at
each percentile (table in `summary.md`). Every sample's glyph was found in the grid at paint
(`verified`), and in all of these runs the next `%output` was the echo itself.

Calibration against the X server (`pixel` scenario, 200 keys, raw X11 `GetImage` polling, no
misses, 1 idle pane only): key to changed pixels 12.31 / 19.76 / 20.44 ms. Pixels appeared
0.84 / 1.03 / 1.16 ms after `Window::present` returned, and never before it.

Read: leg (a) is about 0.4 ms and covers the writer thread, tmux and the shell's echo. Of the
10.9-15 ms median, roughly 7-8 ms is waiting for the next frame tick.

### S2: flood, 8 panes, 60 s

The flood writes unique hex text per line, so GPUI's frame-to-frame line layout cache cannot
serve it.

| | 7 of 8 flooding, 60 s | all 8 flooding, 66 s |
|---|---|---|
| Throughput delivered to terms | 9.9 MB/s | 8.1 MB/s |
| Tick-driven frames | 3740, interval 16.00 / 16.91 / 17.80 ms | 4128, 15.99 / 16.87 / 17.77 ms |
| GPUI build (CPU) | 1.80 / 2.18 / 2.41 ms | 2.24 / 2.63 / 2.95 ms |
| Present (software) | 5.89 / 6.53 / 7.35 ms | 6.71 / 7.42 / 8.24 ms |
| Total > 16.7 ms | 0 | 0 |
| Frame starts < 8 ms apart | 0 | 0 |
| Longest gap between frame starts | 80 ms | 48 ms |
| RSS | 264 to 264 MB | 291 to 290 MB |
| App CPU | 292% | 308% |
| tmux server CPU | 95% | 95% |
| `%pause` events | 52 | 88 |

The throughput ceiling is at or near tmux with the input client attached. Headless controls
with no GPUI, 3 runs each:

| Control (8 panes, 20 s) | Throughput, MB/s | `%pause` |
|---|---|---|
| `tmux -C` only | 50.9, 52.0, 51.0 | 0 |
| plus a `Term` per pane | 35.4, 36.5, 36.0 | 0 |
| plus the second, `no-output` control client used for input | 9.6, 9.5, 9.4 | 24 |

With all 8 panes flooding, the GUI delivers 8.1 MB/s against 9.5-9.6 for the headless
control. That gap is not a measured GUI cost: the runs differ in length (66 s against 20 s),
column count, typing load, and the control's reader starts 3 s late. During flood the feed thread spends 82-84% of its time in
`read()` waiting for tmux and 0.07% on the term lock, and the tmux server is at about 95% CPU,
which points at tmux, though I did not profile tmux. Holding a second control client open cuts
throughput to about a quarter; I did not find out why, and it matters because the real host
uses that arrangement.

With pane painting off and no typing (`--no-paint-panes`, 60 s): 11.4 MB/s and 20% app CPU,
7 panes flooding. It differs from the 7-of-8 run in painting and typing.
That is not a measurement of what painting alone costs; its cause is not isolated.

Throughput counts what reached the terms. `%pause` discards what tmux had queued, so the
terms do not equal what the panes produced; see defects below.

Typing:

- In a quiet pane while the other 7 flood: the table above, n = 1308.
- Into a pane that is itself flooding: 1 sample completed (1740 ms to the byte, 1756 ms to
  present, glyph not verified), and **8 keys timed out at 8 s.** Each had a `%pause` of that pane
  during its wait, but that carries little information: 88 pauses across 8 panes is about one
  per pane every 6 s. tmux discards queued output on pause, so an echo may have been lost or
  may only have been late; nothing here distinguishes them. There is no sample set to take
  percentiles from. The `echoLagProbe`
  semantics read 0.46 ms, because "the next `%output`" is flood output and not the echo: that
  probe is blind in exactly this condition, so compare against `echo byte delivered`. The echo
  comes from the pty line discipline here, not a shell, because `flood-writer` is the
  foreground process.

Frame coalescing: frame starts were never closer than 8 ms on the tick path. GPUI also draws
inside `dispatch_key_event` when the window is dirty, before the key is routed: 1309 such draws
for 1308 keys under flood, counted apart from the frames above and costing about 1.8 ms each on
the main thread at that scene size. `present` is not called inside that draw; the next tick can
present its scene without a further draw, and such a present is not logged. These draws also
take damage and shaping off the next tick frame, so tick-frame build is slightly low. This falls before
t0, so it is in the "main-thread wait" column and in neither leg.

Memory: RSS is bounded in this run: flat over 60 s, and over 180 s (`flood-rss-thp` 262 to
263 MB, `flood-rss-nothp` 259 to 259 MB, `idle-rss-*` flat). In an earlier run of those
scenarios (`results/xvfb-lavapipe/rss-control-earlier-run/`, kept as-is) RSS grew from 263 to
300 MB with THP on, while AnonHugePages grew 2 to 56 MB and then plateaued, and the same
scenario with THP off stayed at 260 MB. So on this machine (THP `always`, scan 10 s,
`max_ptes_none` 511) step growth can be khugepaged collapsing pages rather than allocation.
That explanation rests on that one run; the growth did not recur in the current run. RSS
includes lavapipe's allocations, and tmux server memory is not measured. A 10,000-line
scrollback per pane saturates within seconds, so "flat" is steady state. The flood is hex text, so
this does not exercise glyph atlas growth or font fallback. 180 s does not rule out a slow leak over hours.

### S2: resize and switch

| Measurement (8 panes) | n | ms |
|---|---|---|
| Resize, near-empty panes: `Window::resize` to the first frame with the new cell size and tmux's layout confirmed | 30 | 15.08 / 23.70 / 24.68 |
| Resize, panes filled with 400 lines | 30 | 32.92 / 43.64 / 44.46 |
| Resize, 7 panes flooding | 10 | **4153 / 4827 / 4827** |
| Tab switch: `select-window` sent to first frame with that window active | 60 | 14.04 / 19.08 / 19.74 |
| Pane switch: `select-pane` sent to first frame with that pane focused | 60 | 9.80 / 18.49 / 19.10 |
| Tab switch, 7 panes flooding | 8 | **1547 / 4901 / 4901** |
| Pane switch, 7 panes flooding | 8 | **1675 / 4977 / 4977** |

Iterations start at random phases of the frame tick. Resize runs only at 8 panes. Switch is a best
case: panes are near-empty, row caches are warm, and every other tab switch lands on a one-pane
window. The quiescent column in `summary.md` is
"last frame repainting content in the next 400 ms": idle panes make no output after the
resize, so for them it is by construction the settle time, and it is not useful under flood.
Typing during a resize is not measured.

Under flood, layout and focus are seconds behind. The likely reason is that tmux delivers those
notifications in the same control stream as the panes' output; I did not isolate that. This is measured on this tmux path; whether
xterm-based Muxflow shows the same delay is for the T0.7 comparison. p50 and p95 here are n=8-10
and are indicative only.

### Idle, and GPUI's cost when one pane changes

Idle 8 panes: 0.50% CPU, 170 MB RSS, 0 frames over 30 s. These are lower bounds for the real
app: the spike has no cursor blink (the app sets `cursorBlink: true`), so a blinking cursor's
periodic whole-window presents are not counted. A 1 Hz memory sampler runs inside the process.

One pane changing with 7 other panes filled with text, 20 s:

| | View cache on (`Entity::cached`) | View cache off |
|---|---|---|
| Pane views rendered per frame | 1.0 | 8.0 |
| Panes prepainted per frame | 1.0 | 8.0 |
| Rows reshaped / reused per frame | 1.0 / 0.0 | 1.0 / 7.0 |
| GPUI build, ms | 0.54 / 0.74 / 0.77 | 0.90 / 1.11 / 1.23 |
| Present, ms | 5.84 / 6.47 / 6.80 | 4.61 / 5.11 / 5.36 |
| App CPU | 130% | 124% |

Does one changed pane rebuild the whole window? With `Entity::cached` views, only that pane's
view renders and prepaints. The root re-renders and the whole window's scene is presented every
frame; GPUI has no partial present. The CPU build difference is small: 0.54 ms against 0.90 ms
at 8 panes. Present differs between these two runs by about 1 ms, with one run each: the cause is
unknown, and this does not show that the cache changes present.

### S5, terminal half

Exercised by direct calls into the `EntityInputHandler` on the view, not a real input method.

- Preedit `ni` and then `你` render at the cursor (`ime.json.preedit*.png`).
- Commit clears preedit and sends the bytes through tmux; the echo was found in the grid
  (`commit_echoed_in_grid: true`).
- `bounds_for_range` returned `x:84, y:4, w:15.6, h:18`, captured with `ni` as preedit, so the
  width is two 7.8 px advances. It returns the caret position and ignores the range it is
  given. A real IME's candidate-window placement is untested.
- The test environment has no CJK font, so CJK cells render as tofu. Width and placement are verified;
  glyph appearance is not.

Ligatures: with default font features 5 of 6 test strings (`->`, `!=`, `==`, `=>`, `<!--`) ligate
(glyph-id comparison, `ligature.json`); with the terminal's features they do not.

## Defects found (QA, frames under the task's scratchpad, reproduced where noted)

These are correctness defects in the spike, or findings about the path it uses. None is
a performance number, and all of them bear on S2.

1. **Panes do not recover after a `%pause`.** tmux discards a paused pane's queued output (the
   host's `flow_control.rs` documents this, and the host captures on pause; this spike only sends
   `continue`). After a flood stops, panes can stay on the frozen flood tail, and a later `echo`
   lands at the stale cursor. Reproduced by `pause-control.sh`: with `pause-after=5` a pane has
   no `^C` or prompt (`results/xvfb-lavapipe/pause-control/pause.png`); with pause off, all panes
   do (`nopause.png`). Real `terminal-engine` must port the capture-on-pause path.
2. **A TUI resized during flood can be corrupt after the flood ends.** QA saw nano with a
   missing title bar and help rows split, lasting 16 s until a repaint fixed it. `pause-control.sh`
   shows nano's help bar missing in the frame while tmux has it, in both the pause and no-pause
   runs (`pause-control/*.png`, `*-nano.txt`). So this is not caused by `pause-after`. Root cause
   is not established. The Term is resized on the feed thread in stream order at `%layout-change`,
   so ordering inside the spike is not the obvious culprit; output crossing the layout change on
   tmux's side is a candidate I did not verify. The host reconciles via capture.
3. **Keys can reach the wrong pane while focus lags.** After `select-window` or an external
   `select-pane` under flood, typed bytes went to the previous pane. The spike learns focus from the
   notifications that lag, which is the likely mechanism. A real client needs its focus state to be local, not
   learned back through a backlogged stream.
4. **Layout and focus are seconds behind tmux under flood**, as measured above.
5. tmux zoom is not honoured (the spike uses the tiled layout). Double underline, overline and
   blink are unimplemented. Box characters are font glyphs with faint seams when zoomed.

Rendering at rest was correct in QA: glyphs, colors, attributes, reflow after resize, typing into
the right pane, btop and nano, alternate-screen restore, `split-window` and `kill-pane`.

## Method: where each timestamp is taken

All timestamps are `Instant::now()` in-process, single clock.

- **Key dispatch, t0**: in `PaneView::send_input` (`src/view.rs`), the one choke point for typed
  bytes, immediately before the byte goes on the channel to the input-writer thread. The key
  arrives via `Window::dispatch_keystroke` in-process. **Excluded by design**: OS input queue, X11
  event read, and GPUI work before the handler runs, including the dirty-window draw above. The
  driver's own deadline-to-t0 is reported as "main-thread wait", separately, and is in neither leg.
- **Leg (a), next output**: on the feed thread when the next `%output` record for that pane has
  been parsed, before the bytes are written to the `Term`. This is the `echoLagProbe` semantic.
  **Echo byte delivered** is the same point, for the first `%output` containing the typed byte.
- **Leg (b), frame paint**: in `TerminalElement::prepaint` of the first frame whose snapshot
  generation is at least the generation in which the echo was applied.
- **Leg (b), frame present**: a deferred effect scheduled during paint. It runs after
  `Window::present` returned, on the same thread, because `cx.defer` runs after the draw update's
  effects flush.
- **Tick wait**: echo applied to the start of that frame's build. Build start is the root's
  `render`.
- **Frame build**: root render start to the end of paint, by a canvas sentinel as last child.
  **Frame present**: end of paint to `present` returning.
- Keys are typed one at a time: the next key waits for the previous key's frame or its timeout,
  with a random gap. This closed loop cannot show queueing from fast typing, so it is optimistic
  under flood. Keys are typed at a shell prompt, so this is shell echo except in the flooded pane
  noted above.

## What software rendering distorts

- **Present time is CPU rasterization.** lavapipe uses all 16 cores (about 16% each during flood).
  App CPU of 292% is mostly this; the no-paint flood is 20%, with the caveat above about what
  differs. It says nothing about GPU memory or driver costs.
- **The tick wait follows the refresh.** The GPUI X11 backend uses a timer at the RandR rate, and
  16 ms when RandR reports no dot clock, as Xvfb does. A 60 Hz panel gives about the same wait. A
  faster display shortens it, and Wayland uses frame callbacks instead.
- **There is no compositor and no vsync.** Xvfb shows pixels when the X server has them. A real
  compositor adds buffering and scanout on top, which this measurement cannot see.
- **The inactive-window throttle is disabled.** Xvfb has no window manager, so the window never gets
  focus and GPUI's default 33 ms `inactive_frame_interval` would apply. The spike sets
  `inactive_frame_interval: None` so the measurement reflects an active window.
- **The present figure is a lavapipe swapchain**, not a GPU driver's.

Absolute (b) numbers will not transfer to a real display. What should transfer: that tmux and the
control parser take well under a millisecond at idle, that GPUI's CPU build is about 2 ms under
flood at this 1280x800 window with unique text, and that frames coalesce. RSS should not be assumed
to transfer, since it includes lavapipe.

## Limits

- **No xterm comparison**, by instruction. S1 and S2 pass criteria remain open.
- **No real GPU, no Wayland, no real compositor.** The binary and `measure.sh` run on another
  display (`SPIKE_USE_CURRENT_DISPLAY=1`), and emit the same JSON, except: the `pixel` calibration
  needs no-auth X11 and a window at the root origin, so it may not run there; and PNG capture in
  `ime` is skipped there, so the owner's screen cannot land in `results/`.
- No Muxflow daemon or host hop: tmux is local on an isolated socket. The real path adds the
  host bridge, flow control, and capture-based recovery.
- No history seeding, scrollback UI, selection or links. Panes start with a fresh shell.
- Key dispatch is in-process, not through the OS input path, in every scenario.
- Full-screen TUI performance is not measured; correctness only, in QA.
- No cursor blink is implemented. The 1 pane layout is not covered by resize.
- `environment.txt` records the commit before these uncommitted spike changes.
- Two controls are not part of the 20 s repeats: `pause-control.sh` is one run each way, and
  `rss-control-earlier-run` is a retained copy of one earlier run.
- The machine is shared. Runs are load-gated but not isolated; a load spike during a 60 s scenario
  can still move its numbers.

## What would change this recommendation

Toward "stop" or "rework":

- T0.7 shows xterm's p95 key-to-glyph meaningfully below ours on the same machine.
- T0.7 shows xterm-based Muxflow handles resize or switch under flood substantially better than the
  1.5-4.9 s measured here, or that typing into a flooded pane completes where it timed out here.
- On a real GPU, present plus compositor pushes key-to-glyph past xterm, or frame time exceeds the
  refresh interval at 8 panes.
- Wayland frame callbacks behave differently from the X11 timer in a way that hurts latency or
  coalescing.
- A real IME cannot place its candidate window from `bounds_for_range`.
- A port of the host's capture-based recovery cannot fix defects 1 and 2 at acceptable cost.
- A long soak finds real allocation growth.

Toward "ship it": a real-GPU rerun of `measure.sh` beside the T0.7 baseline, with both below or equal
to xterm at p95, and defects 1-3 fixed in `terminal-engine`.

## Running it

`spikes/gpui/terminal/measure.sh [OUT_DIR]`. Set `SPIKE_ONLY=<regex>` for a subset and
`SPIKE_USE_CURRENT_DISPLAY=1` for a real display. It builds the release binary, runs the scenarios on
the isolated `muxflow-qa-t04` tmux socket, and writes one JSON per scenario, `summary.md`, and `load.txt`.
It exits non-zero if any scenario failed. `spikes/gpui/terminal/pause-control.sh OUT_DIR` runs the
pause control. Unit tests for feed and damage: `cargo test -p spike-terminal` in `spikes/gpui`.
