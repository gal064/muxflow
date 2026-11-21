# GPUI S11: macOS and the real host

The real-host GPUI path keeps subsecond tab switches under flood: the two SSH
runs have p50 122/127 ms and p95 152/196 ms. Navigation no longer takes the
original spike's 1–5 seconds. This is a useful S11 result, but **Phase 2 is not
cleared**: all four GPUI resize runs hit a repaint timeout, IME is unverified, and the exact macOS dependency pin needs a stable-Rust workaround.
Several Tauri/switch calibrations also fail, leaving explicit comparison gaps.

Test machine: Apple M1 Pro, 8 logical CPUs, 16 GiB RAM, macOS 26.5.1,
Retina scale 2. Mac local tmux is 3.7b; the earlier Linux report used 3.8.
Absolute cross-OS comparisons therefore also differ in tmux version. Rust is the repository-pinned 1.97.1. The desktop is the packaged
release build; the spike is a release build using the existing Alacritty grid.

The SSH target is a new Ubuntu 24.04 Docker container (tmux 3.4), not exposed publicly. Both applications connect as `ade` through the private
`muxflow-s11-container` SSH alias. Neither application connects to the owner's account.
The Ubuntu base replaces the transport fixture's Debian base because the host
built on the Linux host requires glibc 2.39. Floods run inside this container.
The host was built only in a dedicated worktree, with
`CARGO_BUILD_JOBS=5`. Both SSH frontends use that same host executable; local
runs use the helper built with this tree's macOS package and a private tmux socket.
The helpers were built from integration `a4d3ef79`; all production desktop, host
and `crates/` sources are identical in the final S11 tree. The Linux release
worktree binary, the Mac's Linux-helper copy, and the container helper have the
same SHA-256.

## What changed

`spike-terminal host` starts `muxflow-host bridge --stdio` over SSH, exchanges
existing protocol messages, subscribes to topology, attaches panes, and parses
terminal seed/output bytes into the existing `alacritty_terminal` grids. It
acknowledges admitted bytes/records and sends existing input, visibility,
action and window-resize requests. No protocol or host implementation changed.
This is disposable spike code: no reconnect, replay, tail cache, or real engine.
A host stall or resnapshot request ends the reader instead of attempting
recovery. Those limitations matter under flood.

## Why the Tauri path is fast

The original spike sends `select-window`, `select-pane`, and client sizing down
the **output control client's stdin** (`terminal/src/control.rs`). With another
control client attached, flooded output can hold this command/reply observation path up for seconds.

The real host constructs a **fresh tmux command process** for `SelectWindow` and
`FocusPane` (`apps/host/src/service/tmux_actions.rs`). Its window-sizing path
also uses a fresh command, `tmux run-shell -C`, to apply the sizing claim
(`apps/host/src/service/terminal.rs`). The dedicated persistent input client
therefore does not imply that navigation uses the congested output stdin.
Tauri also updates selection optimistically and has the host's bounded terminal
flow and recovery machinery. The spike in this branch waits for authoritative
host topology and does not reproduce the full renderer recovery implementation.

A no-GUI container probe with two control clients recorded:

| Command route | Six observations, ms |
|---|---|
| Output control stdin | 4663, 2432, 1320, 711, 389, 200 |
| Fresh tmux process | 13.4, 11.5, 13.9, 11.5, 12.6, 13.6 |

This isolates an important mechanism, not a steady-state GUI benchmark. The
probe uses `pause-after=5` and does not resume paused output, so the falling
stdin delays must not be read as a steady-state distribution. The existing
Linux report's two-client slowdown is real; merely counting control clients
misses which command path the frontend uses.

## Measurement method

`tests/performance/runtime/macos/pixels.swift` is one unchanged instrument for
both apps. It posts genuine HID keys/clicks, captures the exact application
window through ScreenCaptureKit, and observes pixels. Capture requests 120 fps;
that is not a claim of 120 distinct display frames or a photodiode measurement.
The meter executable SHA-256 is
`ca015f96a4d5c6ec07d6dcdbc901ba525ce17e358802c19b73bdb20e98c7c20b`.
Both apps use equal 1280×800-point outer windows at scale 2. Native chrome and
terminal cell geometry still differ and the spike has no full application UI.
Each fixture also has a hidden second window for tab switching; the one-pane
row means one visible pane. Runs use Tauri then GPUI, twice per endpoint.

Typing uses a calibrated quiet text band, randomized gaps, closed-loop samples,
and a multi-color glyph-change filter to reject cursor blinks. A timeout ends
the scenario so a late echo cannot be credited to the following key. Self-flood
uses the Linux report's fixed-cell echo sentinel, with flooding everywhere else
in that pane. The same Rust flood writer is used for ordinary floods. The solid-color
sentinel polls terminal size every 20 ms, which can add up to 20 ms to a resize
result relative to the Linux signal-driven painter; both Mac apps use the same
painter. Self-flood starts after the frontend has sized the panes, with the other
seven panes flooding too. The test aborts at the first key timeout; remaining
keys are reported as unattempted, rather than crediting late echoes.

The no-GUI stdin probe times receipt of a marker, so it does not distinguish
command execution delay from backlogged reply delivery.

Resize uses native AX window-size changes.
Switch and resize clocks start at the Python driver request and include the
meter baseline-frame and IPC overhead;
typing clocks start at the instrument's HID post. Tab tests use shared Ctrl+Alt+F5/F6
bindings (an override only in Tauri's disposable profile). The expected painted
window, focused cursor picture, and calibrated resized content boundaries are
checked rather than crediting arbitrary flood animation. Failed calibrations
and timeouts are reported separately. Diagnostic runs made while developing
the instrument are retained and excluded from final comparisons. Percentiles
use nearest rank: each typing scene has 120 keys; switch/resize scenes have only
12/10 planned observations, so their p95 and p99 often equal the maximum.
Timeouts are 5 s for ordinary typing, 15 s for switches/resize, and 8 s for
self-flood.

Memory comes from a single native `footprint` group invocation and its
`total footprint` field, rather than adding process RSS. The raw group report
preserves shared-page accounting. Footprint is memory charged to the group,
including compressed/swapped attribution; it is not resident RSS. CPU is process CPU time over the flood
interval, with 100% meaning one logical CPU. The capture helper and WindowServer
are excluded. The capture stream remains active for both
apps during resource sampling, with CPU readback reduced to one pixel.
The remote host/container and the tmux/flooder processes are outside Mac
frontend memory/CPU totals. This differs from the Linux report, which includes
the local host in Tauri's total. Local
private host processes are included. Both SSH terminal streams use compression. Tauri additionally sets
`IPQoS=lowdelay` and pools SSH connections; the spike uses a plain SSH process.
These transport differences are not separately isolated.

Graphics-tagged footprint categories are
only an approximate unified-memory attribution, not a separate VRAM counter.
Resource totals establish the cost of these process groups under this workload;
rendered throughput and identical retained output were not measured. In
particular, lower CPU alone does not establish equivalent flood rendering.
The apps ran serially without
concurrent builds or our other GUI checks.

## Build and platform checks

The unmodified exact GPUI macOS pin fails to compile on Rust 1.97.1: three uses
of unstable `Option::map_or_default` in `gpui-pre-macos` 0.3.11. The disposable
`spikes/gpui/prepare-macos.sh` copies the Apache-2.0 platform crate into ignored
build output and replaces those three calls with equivalent stable calls.
No pin was bumped. With that explicit local patch, release workspace build and
workspace tests pass: 58 Markdown tests and 23 terminal tests, plus the existing
headless render check. A clean upstream macOS build still fails without it.

The proper license command, from `spikes/gpui`, passes:

```sh
../../tmp/s11/cargo-deny/bin/cargo-deny --config ../../deny.toml check licenses
```

The packaged Tauri build passes using `ADE_MACOS_PACKAGE_SMOKE=1` (native macOS
package; cross-compiled Linux helpers omitted), `CARGO_PROFILE_RELEASE_DEBUG=0`,
and the configurable free-space threshold set to 8 GiB. The Docker host helper
is supplied explicitly from this tree. This does not verify a distributable,
notarized, universal package or cross-platform helper packaging.

## macOS check status

| Check | Status | Evidence and limits |
|---|---|---|
| S6, unmodified exact pins on Rust 1.97.1 | **Fail** | Three unstable `map_or_default` calls in `gpui-pre-macos` 0.3.11 |
| S6, build/test with explicit local workaround | **Pass** | Final release workspace build; 58 Markdown and 23 terminal tests; initial headless render 1440×840, 654 colors |
| S6, license allowlist | **Pass** | `cargo-deny` reports `licenses ok`; policy unchanged; no forbidden source opened |
| Run `spike-shared` | **Pass** | Rendered fixture; independent counter captures 0 → 1 → 2 |
| Run `spike-terminal` | **Pass** | Release view exercised through the real host, both private Mac tmux and Docker SSH |
| Run `spike-markdown` | **Pass** | Sample document rendered, including table/code. This is a launch/render check |
| S3, title bar and window movement | **Pass** | Native title and controls visible; real title-bar drag moved the window exactly 100×50 points |
| S3, native window controls | **Pass** | Minimize/restore, fullscreen entry/green-button exit, and close independently verified |
| S3, custom client decorations | **Not run** | Probe requests `Client` but reports `Server`; the checks above cover the native title bar |
| S3, Retina | **Pass** | Probe reports scale 2; exact-window captures have twice the point dimensions |
| S3, clipboard both ways and restoration | **Pass** | Probe-out read by native clipboard, probe-in read by GPUI; final test restored its starting state |
| S3, file from Finder | **Pass** | Real desktop drag; probe logged the exact disposable file path |
| S3, path prompt | **Pass** | Native panel opened, returned the selected fixture path; cancellation also exercised earlier |
| S10, find/read button | **Pass** | CUA exposes `Press me`, role `AXButton`, action `AXPress` |
| S10, element-token press | **Pass** | Accessibility route; independent pixels show counter 0 → 1 |
| S10, window-coordinate click | **Pass** | Final run used `global_input`; independent pixels show counter 1 → 2. Both routes work here, unlike Linux's token refusal |
| S5, terminal real IME/candidate caret | **Not run** | Real IME was not exercised |
| S5, Markdown real IME/candidate caret | **Not run** | Real IME was not exercised; direct synthetic preedit is not counted |

The bare probe exposes no application menu. Control-Command-F did not leave
fullscreen; clicking its native green button did. Native title-bar behavior
therefore passes the exercised controls, while application-menu shortcuts and
custom client chrome remain separate work.

The final bounded check saved the clipboard state in memory, verified both
directions, closed the writing probe, and restored that state. Rich, image and
multi-item restoration was not exercised, and no clipboard contents are in the
results. Later platform checks used `PROBE_NO_CLIPBOARD=1`.

CUA's action response alone often says `unverifiable`; the
passes above depend on the subsequent counter, geometry and probe-log readback.

## Disconnect and restart

Only the S11 container was briefly paused, and only SSH descendants of each
newly started test app were terminated. After unpausing, the same session ID,
pane ID, pane PID, cursor and captured text were unchanged: the offline key had
not reached the pane or replayed during the following ten seconds.

Both frontends were then restarted without replacing tmux. The next genuine
key reached the **same pane process**. GPUI also repainted it within 53 ms.
Tauri's server echo was present, but its calibrated pixel wait timed out at
3 s after restart, so the full Tauri journey is recorded as **Fail**. GPUI's server/restart check is **Pass**. Its window
stays open when the bridge ends; offline UI pixels can change, but the key is
not delivered or replayed. This does not verify an input-disabled UI or automatic
reconnect/recovery in the disposable spike.

## Results

### SSH

Latency in ms, p50 / p95 / p99. Each ordinary typing cell is 120 keys with zero timeouts.

| Scenario | Tauri 1 | Tauri 2 | GPUI 1 | GPUI 2 |
|---|---:|---:|---:|---:|
| 1 pane idle | 43.9 / 85.4 / 152.2 | 42.5 / 62.4 / 87.1 | 57.0 / 67.8 / 69.8 | 58.3 / 73.8 / 129.2 |
| 8 panes filled | 43.4 / 54.0 / 65.9 | 45.5 / 60.5 / 107.7 | 55.0 / 69.4 / 83.7 | 58.5 / 69.1 / 80.1 |
| Quiet pane, 7 flooding | 74.1 / 106.5 / 126.6 | 73.3 / 111.7 / 127.1 | 79.4 / 107.3 / 139.3 | 82.1 / 108.3 / 139.3 |
| Tab switch, 7 flooding | Calibration failed | Calibration failed | 121.7 / 152.4 / 152.4; 12 done, 0 TO | 126.5 / 196.1 / 196.1; 12 done, 0 TO |
| Pane switch, 6 flooding | 56.2 / 78.7 / 78.7; 12 done, 0 TO | Calibration failed | Calibration failed | 78.0 / 151.2 / 151.2; 12 done, 0 TO |
| Resize, 7 flooding | 400.3 / 458.2 / 458.2; 10 done, 0 TO | 346.4 / 416.6 / 416.6; 10 done, 0 TO | 0 completed; 1 timeout | 239.1 / 278.8 / 278.8; 6 done, 1 TO |

Self-flood, all eight panes flooding: 20 keys planned, stop at first 8 s timeout.

| App/run | Completed / attempted | Wall time, s | Timeout |
|---|---:|---:|---:|
| tauri 1 | 3 / 4 | 26.6 | 1 |
| tauri 2 | 3 / 4 | 26.6 | 1 |
| gpui 1 | 1 / 2 | 14.5 | 1 |
| gpui 2 | 20 / 20 | 37.1 | 0 |

Eight flooding panes, 60 requested one-second samples. Memory is group footprint in MiB, counting shared pages once. Graphics attribution is included in footprint.

| App/run | Actual seconds | CPU % | Footprint start → end, MiB | Graphics end, MiB |
|---|---:|---:|---:|---:|
| tauri 1 | 63.9 | 189.3 | 571.7 → 620.1 | 125.8 |
| tauri 2 | 63.7 | 196.7 | 742.5 → 535.9 | 110.0 |
| gpui 1 | 63.2 | 64.4 | 265.1 → 258.3 | 84.2 |
| gpui 2 | 63.5 | 23.1 | 265.0 → 171.4 | 83.6 |

### LOCAL

Latency in ms, p50 / p95 / p99. Each ordinary typing cell is 120 keys with zero timeouts.

| Scenario | Tauri 1 | Tauri 2 | GPUI 1 | GPUI 2 |
|---|---:|---:|---:|---:|
| 1 pane idle | 34.0 / 49.0 / 67.5 | 37.5 / 51.2 / 58.5 | 49.9 / 61.8 / 68.7 | 52.0 / 72.3 / 893.7 |
| 8 panes filled | 37.6 / 52.7 / 72.5 | 37.9 / 56.3 / 63.1 | 52.0 / 60.3 / 68.5 | 50.4 / 94.6 / 158.0 |
| Quiet pane, 7 flooding | 133.0 / 194.6 / 309.1 | 124.8 / 169.2 / 201.0 | 96.2 / 168.8 / 210.0 | 88.9 / 154.6 / 186.4 |
| Tab switch, 7 flooding | Calibration failed | Calibration failed | 119.9 / 224.9 / 224.9; 12 done, 0 TO | 137.3 / 220.0 / 220.0; 12 done, 0 TO |
| Pane switch, 6 flooding | 91.2 / 121.5 / 121.5; 12 done, 0 TO | 84.1 / 121.9 / 121.9; 12 done, 0 TO | 80.3 / 146.3 / 146.3; 12 done, 0 TO | 81.0 / 136.5 / 136.5; 12 done, 0 TO |
| Resize, 7 flooding | 644.3 / 785.1 / 785.1; 10 done, 0 TO | 697.5 / 889.7 / 889.7; 10 done, 0 TO | 0 completed; 1 timeout | 896.4 / 1278.4 / 1278.4; 8 done, 1 TO |

Self-flood, all eight panes flooding: 20 keys planned, stop at first 8 s timeout.

| App/run | Completed / attempted | Wall time, s | Timeout |
|---|---:|---:|---:|
| tauri 1 | 1 / 2 | 11.3 | 1 |
| tauri 2 | 0 / 1 | 8.3 | 1 |
| gpui 1 | 20 / 20 | 47.1 | 0 |
| gpui 2 | 20 / 20 | 45.3 | 0 |

Eight flooding panes, 60 requested one-second samples. Memory is group footprint in MiB, counting shared pages once. Graphics attribution is included in footprint.

| App/run | Actual seconds | CPU % | Footprint start → end, MiB | Graphics end, MiB |
|---|---:|---:|---:|---:|
| tauri 1 | 63.9 | 256.1 | 728.0 → 602.9 | 124.2 |
| tauri 2 | 64.5 | 249.5 | 720.9 → 4786.9 | 127.6 |
| gpui 1 | 63.6 | 330.0 | 278.1 → 265.9 | 84.2 |
| gpui 2 | 63.1 | 424.3 | 278.0 → 266.0 | 84.2 |

### SSH echo round trip

Persistent SSH channel, idle before each series, 30 samples. Excludes authentication and new-process startup.

| Series | p50 / p95 / p99, ms |
|---|---:|
| tauri 1 | 6.60 / 11.44 / 12.90 |
| tauri 2 | 6.41 / 10.50 / 12.63 |
| tauri corrected-1 | 5.71 / 10.02 / 14.81 |
| tauri corrected-2 | 6.37 / 9.91 / 10.00 |
| gpui 1 | 6.24 / 8.55 / 15.08 |
| gpui 2 | 5.95 / 9.92 / 13.04 |
| gpui corrected-1 | 5.54 / 8.71 / 9.42 |
| gpui corrected-2 | 5.59 / 9.23 / 16.86 |

### Reading the results

GPUI's host-backed tabs stay fast in both SSH runs and both local runs. The
single calibrated SSH pane run is also fast; its other repeat could not
calibrate the inactive cursor. Local pane medians are similar to Tauri, with
higher GPUI tails. This establishes the navigation-path improvement, while the
missing calibrations prevent a complete same-scenario Tauri comparison.

All four GPUI resize runs hit the 15 s content-boundary timeout: SSH completed
0/1 and 6/7 attempted resizes; local completed 0/1 and 8/9. Reported successful
percentiles exclude the timed-out observation and must not hide that failure.
Tauri completed all ten in each run. The GPUI resize timeout's cause was not
isolated. Initial exact-mask SSH resize diagnostics
were replaced by the common two-boundary criterion before the final comparisons.

The Tauri tab check failed before flood in all four runs. tmux selected `@1`
and reported a full 157×44 pane, but the expected yellow region was absent;
one diagnostic capture showed only a colored row. No trustworthy Tauri tab
pixel latency is available. The SSH pane calibration also failed once per app:
the inactive cursor was absent or unstable. The cause of these calibration
failures was not isolated.

SSH GPUI frontend groups use less CPU and footprint here. Locally, including
the private helper, GPUI uses **more CPU** (330/424% versus 256/250%) and less
footprint. One local Tauri repeat grows from 721 to 4,787 MiB (4.67 GiB) in about
a minute; the raw group has no footprint errors, and most end attribution is
WebContent (WebKit malloc and JS VM Gigacage, with substantial swapped memory).
The other local Tauri repeat shrinks. This is an observed outlier, not a diagnosed
leak. The GPUI local idle run's 894 ms p99 is also retained. Two repeats on shared
machines are insufficient to isolate these variations.

Self-flood wall time includes randomized inter-key gaps and the first timeout.
Unattempted keys are not counted as dropped. A timeout cannot distinguish a
lost echo, a delayed echo, and a retained/rendered-output problem.

Raw results are not published.
The `corrected-1/2` SSH files supply resize, and Tauri corrected-1 also supplies
its first all-eight-pane self-flood test. The original Tauri SSH-1 self-flood
used only one flooder and is excluded. Preliminary local runs with an agent
setup modal are excluded.


## Recommendation and remaining verification

Continue the host-based experiment and Phase 1 extraction, but keep the Phase 2
gate closed. Fresh tmux action commands remove the old navigation bottleneck.
The result does not establish the plan's “no worse than xterm” typing/resize
gate: GPUI's idle typing is slower in several comparisons, resize has a timeout,
and the Tauri tab sentinel fails calibration. Both frontends stall under
self-flood: GPUI completes all 20 keys in three of four runs, while Tauri hits
a timeout in all four valid runs.

S5 was not run. Real composition and a
candidate window at the caret remain required in **both** terminal and Markdown.
The exact-pin macOS build failure also needs an upstream/pinning decision before
a clean build gate can pass.

Exactly what remains unverified:

- Real IME composition, candidate-window position, and CJK input in both spikes.
- macOS 14, Intel Macs, other GPUs/displays, and hosted macOS CI. This run uses
  one M1 Pro on macOS 26.5.1 at scale 2.
- A full engine, automatic reconnect/replay/resnapshot recovery, VT parity and
  complete TUI correctness through resize. The boundary sentinel checks two
  painted edges, rather than the entire terminal grid.
- Successful latency numbers for scenarios whose calibration failed. These
  failures do not identify whether the harness, host seed, or frontend caused
  the missing/stale picture.
- Physical key-to-photon latency, delivered capture rate, and input beyond the
  measured sample counts. Switch/resize and typing also have different clock
  starts, described above.
- Remote host/container CPU and memory, separate device-wide GPU memory, equal
  flood throughput, output retention parity, and a long memory-growth soak.
  Start/end footprint over about a minute cannot establish bounded long-term
  memory.
- A notarized/universal production package and Linux helper cross-compilation
  from the Mac. The tested package uses the native smoke build plus the explicit
  Linux-worktree helper.
- SSH RTT under flood: RTT was sampled idle immediately ahead of each SSH
  series, rather than continuously during flood.

Additional limits: real GUI editing/table interaction in Markdown, application
menu shortcuts, custom client chrome, and rich clipboard-format restoration
were not verified. The disconnect journey above is a manual frontend restart,
not a proof of automatic recovery. S10 covers a button label/role and action,
not terminal/editor accessibility text parity.

## Delivery and cleanup

The source branch is `gpui/s11`. No wire messages, protocol fields, production host code,
or desktop code changed.

The test container and its unique image were removed. The final process audit found no
S11 processes left.

On the Mac, the test frontends/probes, SSH children, meter jobs, owned CUA daemon
and caffeinate process are stopped. The private tmux/runtime directories, test
app bundle, private app homes and disposable SSH key/config were removed. The
three task-created Finder windows are closed. Final process and launchd audits
found no remaining task processes/jobs.
The Linux source worktree and ordinary build outputs remain; the unchanged,
granted meter binary remains as an **inactive** ignored build artifact so its
identity can be reused.
