# GPUI migration: Phase 0 report (Linux)

**Recommendation: conditional go.** GPUI clearly beats the current app on memory and on
CPU under load, and matches it on typing at rest. Start Phase 1, which is useful either
way. Before Phase 2, connect the terminal spike to the real host (S11 below) and pass macOS.

Scope: Linux only, local tmux, one machine (an AMD integrated GPU, 16 CPUs). The
spike is a terminal with no panels, editor or host, so GPUI's numbers will rise in the
real app. Nothing here measures a remote VPS. The shipping app is unchanged.

## Spike results

| Spike | Result | Notes |
|---|---|---|
| S1 Terminal latency | **Pass** | p95 19 ms against 22 ms, same probe, real GPU |
| S2 Flood and layout | **Partly** | Typing beside a flood and memory pass. Tab, pane and resize under flood fail in the spike, see below |
| S3 Platforms | **Partly** | X11 clipboard both ways and 2x scale work. Native Wayland opens on the GPU. File drop, path prompts, HiDPI on Wayland and GNOME were not driven |
| S4 Live Markdown | **Pass** | Works on `gpui-base` `editor_host`, not the Kit editor. Large files and accessibility were not prototyped |
| S5 Text input | **Not proven** | Preedit renders at the caret by direct call in both spikes. No real IME was run in the test environment |
| S6 Toolchain | **Pass** | Builds and tests on Rust 1.97.1, licenses clean |
| S7 CI rendering | **Pass locally** | Headless render and frame capture work. The workflow has never run on GitHub |
| S8 Drag out | **Answered** | No drag has to reach the OS |
| S9 VT parity | **Partly** | Wrap, CJK, plain emoji and combining marks pass. VS16 and ZWJ emoji fail, see below |
| S10 Test driver | **Pass, with a gap** | `cua-driver` finds, reads and presses a GPUI button. Token-based clicks are refused, see below |

macOS has not run at all and must pass before Phase 2.

## Measurements

Tauri against the GPUI spike on the real GPU, through Xwayland, one black-box key-to-pixels
probe on both apps, two runs each. Milliseconds, p50 / p95. Raw results are not published.

| Scenario | Tauri | GPUI spike |
|---|---|---|
| Key, 1 pane idle | 15 / 22 | 11 / 19 |
| Key, 8 panes filled | 15 / 22 | 12 / 19 |
| Key in a quiet pane, 7 of 8 flooding | 83 / 145 | 16 / 25 |
| Resize, idle | 115 / 135 | 23 / 40 |
| Tab switch, idle | 45 / 205 | 12 / 20 |
| Pane switch, idle | 25 / 37 | 10 / 20 |
| Tab switch, 7 of 8 flooding | 215 / 350 | **4,100 / 4,950** |
| Pane switch, 6 of 8 flooding | 170 / 200 | **1,140 / 5,000** |
| Resize, 7 of 8 flooding | 305 / 590 | **did not finish repainting** |
| Typing into a pane that is itself flooding | 20 of 20 keys, about 5 s each | **7 and 12 of 20 keys** |

Memory, with shared pages counted once. GPU memory is separate and approximate:

| | Tauri | GPUI spike |
|---|---|---|
| 1 pane at rest | 464 MB, GPU 184 MB | 67 MB, GPU 89 MB |
| 8 panes at rest | 588 MB, GPU 197 MB | 114 MB, GPU 89 MB |
| 8 panes flooding, 60 s | 1.2 to 1.5 GB, GPU 146 MB | 193 MB, GPU 89 MB |

Tauri's figures include the 37 MB `muxflow-host`, which the real GPUI app also needs.

CPU with 8 panes flooding: about 270% against 36%.

Native Wayland against Xwayland, spike only, in-process key to frame present returned,
8 filled panes, p50 / p95: 4.1 / 10.2 ms against 9.4 / 17.1 ms. Under flood they match
(13.5 / 20.2 ms against 13.6 / 20.6 ms), and each frame takes about 2.3 ms of work.

App size: 16.5 MB compressed desktop binary against 10.9 MB today, before the editor and
grammars are added. On Linux, GPUI no longer needs about 195 MB of WebKitGTK and GTK.

## The open problem: interaction under flood

The lag is not caused by GPUI's rendering. With no GUI at all and one tmux control client,
`select-window` is reported in 1 to 3 ms under flood, at 48 MB/s delivered. Add a second,
`no-output` control client and it takes 3.9 to 5 s, at 14.6 MB/s, with panes pausing. The spike and the real host both hold that second client.

Today's app has that client too and still switches in about 0.2 s, and why is not known.
**S11:** connect the spike to the real `muxflow-host` over its protocol, locally and over
SSH, and rerun these scenarios. That settles this question and gives the first remote
numbers. Do this before Phase 2.

Separately, in the spike and needing tests in Phase 3: panes stay frozen after `%pause`
until capture recovery is ported, a TUI resized during flood stayed corrupt (cause
unknown), and keys reached the wrong pane while focus lagged.

## Emoji widths

Cells, tmux against today's xterm (Unicode 6, the default) and `alacritty_terminal`:

| Sample | tmux | xterm | Engine |
|---|---|---|---|
| Plain emoji | 2 | 1 | 2 |
| CJK, two characters | 4 | 4 | 4 |
| `❤️☀️✈️` | 6 | 3 | 3 |
| `#️⃣` | 2 | 1 | 1 |
| `👩‍💻` | 2 | 2 | 4 |
| Family | 2 | 4 | 8 |
| `🏳️‍🌈` | 2 | 2 | 3 |

The engine is better on plain emoji and the same on VS16. It is worse on `👩‍💻` and
`🏳️‍🌈`, where the host's seed path then erases text after the emoji. No fork for now.
T3.1 stops the erase, for example by clearing the row before painting it.

## Test driver gaps

GPUI's AccessKit elements report an empty role and window-relative bounds. `cua-driver`
refuses a click by element token (`stale_element_token`) but presses correctly through a
click at the element's window coordinates. T2.1 sets the accessibility baseline, and
must make token clicks work or adapt the journeys.

## Decisions for the owner

1. Is the bar met? Recommended bar: no regression plus a measurable gain in resources or
   flood behavior. Idle resources meet it. Flood interaction waits on S11.
2. Drop Monaco's language workers. Recommended.
3. Enable `gpui-fast`? Not needed so far.
4. Drop spellcheck in Live Markdown. Recommended.
5. Confirm the per-crate license exceptions: MPL-2.0, BSL-1.0, CDLA-Permissive-2.0, LLVM-exception.
6. Keep `vt-parity-pause-idle` as a gate? It also fails on today's host path, and
   deserves its own investigation.
7. Desktop feature freeze: the plan says from Phase 3. Phase 2 is safer, because the new
   shell starts then.
8. Allow a push of `gpui/integration`, so hosted CI and the macOS build can run.

## What was not verified

Hosted CI, macOS, GNOME, a real IME, file drop, path prompts, Wayland HiDPI, Wayland
clipboard, the physical display, anything remote, and a
long soak of the real app. Frame timing is relative.
