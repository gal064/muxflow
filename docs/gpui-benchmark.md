# GPUI vs Tauri desktop benchmark

**Decision (2026-11-11): commit to GPUI.** The owner accepted it after the Linux run below.

The harness is `tests/performance/app-bench/` (branch `gpui/app-bench`). Both apps are
release builds of the same commit, run against the same local host and the same SSH host,
with the same scenarios. The SSH host is a 4 vCPU Debian 13 cloud VM about 29 ms away.

## Linux headline (Hyprland on amdgpu, 1600x1000 window, medians of 3 runs)

| What | Tauri | GPUI |
|---|---|---|
| Memory, idle (PSS) | 417 MB | 85 MB |
| Memory, 8 tabs x 2 panes | 424 MB | 96 MB |
| Memory, peak under heavy output | 740-843 MB | 125-132 MB |
| First terminal content, local cold / warm | 832 / 785 ms | 230 / 157 ms |
| First terminal content, SSH cold / warm | 1450 / 1294 ms | 1096 / 941 ms |
| CPU, idle | 3.5% | 1.3% |
| CPU, `seq 1 3000000` (CPU-seconds) | 4.17 | 0.87 |
| CPU, 60 lines/s across 16 panes | 41% | 21% |
| CPU, 3 Claude + 3 Codex replays | 22% | 22% before the spinner fix, about 17% after |
| CPU, 6 agents working, window visible | 10.3% | 19.7% before the spinner fix, about 6.5% after |
| CPU, 6 agents working, window hidden | 10.6% | 1.0% |
| Typing p95, local / SSH | 47 / 81 ms | 31 / 67 ms |
| Package (tar.gz, app + helper) | 20.6 MB + 194 MB system WebKit/GTK | 36.1 MB, no web stack |

CPU is % of one core. The host helper, tmux, sshd and the remote machine cost the same for
both apps in every scenario, because they run the same helper.

GPUI uses more GPU time (about 7% busy against 3-5%) when panes are busy. It costs no CPU
and no compositor time.

## The one regression found, and its fix

The agent spinner and the tab pulse asked GPUI for a new frame on every vsync, so 6
working agents cost 19.7% CPU against Tauri's 10.3%. Commit `3c42f6e` steps both on a
shared 13 fps clock. The first post-fix runs (local and SSH, 2 of 3 runs) gave about 6.5%
visible, 1.0% hidden, and 17% for the agent replay. Still to measure: the same 6 agents
with no spinner at all. If the spinner costs more than about 1 point over that, it becomes
a static "working" icon.

## macOS

Pending. The owner runs the same harness (`run-macos.sh`) on an Apple silicon Mac,
local first and then against the same VM.

## Full Linux results (before the spinner fix)


Run started 2026-11-11T18:49:19-0800; 3 runs per cell; steady windows 60.0 s after 15.0 s settle; compositor: private.

Cells: median [min-max] over runs. Ratio = Tauri / GPUI (above 1: Tauri uses more or takes longer).
CPU in % of one core; GPU as % engine-busy time from DRM fdinfo; memory summed over the process tree.

### Notes on this run (Linux)

- **Compositor.** The desktop session had no monitor attached and was locked, so both apps ran on a private nested Hyprland 0.56.2 (same binary and amdgpu renderer as the session) with one headless 1920x1080@60 output, scale 1, no animations or blur, Xwayland off, and no GDK_SCALE. Both windows were floating at 1600x1000. Frames were rendered on the real GPU, but nothing was scanned out to a panel.
- **Cold start** means fresh app state with no helper daemon running. It does not mean a dropped page cache, because that needs root. The binaries were in the page cache for both apps.
- **Captures.** Visible-content checks use `grim` captures through the compositor. Polling costs compositor CPU during startup, heavy output and typing, and it costs the same for both apps. Steady-state windows (idle, stream, agents, spinner) take no captures.
- **Typing** is the time from a keystroke sent with `wtype` (one process per key) to the first capture that shows the echoed cell. It includes wtype start-up and is quantized to the ~16 ms capture interval, so it is a comparison between the apps, not an absolute input-to-photon figure.
- **Agent panes.** In the 3x2 layout the pane is 84x17 cells in GPUI and 84x18 in Tauri, because the chrome differs. The recordings were made at 84x17.
- **Agent recordings** are one real Claude Code session (plan mode) and one real Codex session (read-only sandbox), about 175 s each, answering read-only questions about this repo. They are replayed with their recorded timing, three of each, at staggered offsets. They are not committed.
- **Agent state** (working/done) is driven through the real hook path, `muxflow-host hook ingest`, run under the tmux server. The spinner scenario keeps all six rows working with silent panes. "Hidden" means the window is on another workspace of the nested compositor.
- **Editor** only measures Quick Open plus opening a 5 MB file. Scrolling is not measured, because PageDown did not reach Tauri's editor after it opened and pointer input cannot be injected here. **Explorer is not measured**, because keyboard navigation did not reach the file tree in either app. The evidence screenshots are in `<host>/evidence/`.
- **SSH** goes over the public internet to a cloud VM running Debian 13 with 4 vCPU and 16 GB, about 29 ms ping RTT. That latency dominates SSH typing and SSH heavy-output wall time.
- The session bus (DBUS) of the desktop session was passed through to both apps. Everything else (HOME, helper runtime dir, tmux socket dir) was scratch.

### Sizes (Linux x86_64)

| Binary | As built (MB) | strip --strip-all (MB) | gzip -9 of as-built (MB) |
|---|---|---|---|
| Tauri app (muxflow, frontend embedded) | 28.7 | 21.1 | 11.4 |
| GPUI app (muxflow-gpui) | 89.9 | 67.5 | 26.8 |
| Host helper (muxflow-host, shared) | 14.1 | 10.5 | 4.5 |

Package as the Linux release tarball lays it out (app + helper + per-arch helper copy, tar.gz):
Tauri 20.6 MB; GPUI 36.1 MB.
The release flow builds no GPUI package yet at this commit; its row uses the same layout.

System libraries the app links (ldd), beyond the helper:

- Tauri: /lib64/ld-linux-x86-64 libaom libatk-1.0 libatk-bridge-2.0 libatomic libatspi libavif libblkid libbrotlicommon libbrotlidec libbrotlienc libbz2 libc libcairo libcairo-gobject libcloudproviders libcom_err libcrypto libdatrie libdav1d libdbus-1 libdrm libdw libEGL libelf libenchant-2 libepoxy libevdev libexpat libffi libfontconfig libfreetype libfribidi libgbm libgcc_s libgcrypt libgdk-3 libgdk_pixbuf-2.0 libgio-2.0 libGLdispatch libglib-2.0 libGLX libglycin-2 libgmodule-2.0 libgobject-2.0 libgpg-error libgraphite2 libgssapi_krb5 libgstallocators-1.0 libgstapp-1.0 libgstaudio-1.0 libgstbase-1.0 libgstfft-1.0 libgstgl-1.0 libgstpbutils-1.0 libgstreamer-1.0 libgsttag-1.0 libgstvideo-1.0 libgtk-3 libgudev-1.0 libharfbuzz libharfbuzz-icu libhidapi-hidraw libhwy libhyphen libicudata libicui18n libicuuc libidn2 libjavascriptcoregtk-4.1 libjpeg libjson-glib-1.0 libjxl libjxl_cms libk5crypto libkeyutils libkrb5 libkrb5support liblcms2 liblzma libm libmanette-0.2 libmount libnghttp2 liborc-0.4 libpango-1.0 libpangocairo-1.0 libpangoft2-1.0 libpcre2-8 libpixman-1 libpng16 libpsl librav1e libresolv libseccomp libsecret-1 libsharpyuv libsoup-3.0 libsqlite3 libstdc++ libSvtAv1Enc libsystemd libtasn1 libthai libtinysparql-3.0 libtss2-esys libtss2-mu libtss2-rc libtss2-sys libtss2-tctildr libudev libunistring libunwind libwayland-client libwayland-cursor libwayland-egl libwayland-server libwebkit2gtk-4.1 libwebp libwebpdemux libwebpmux libX11 libX11-xcb libXau libxcb libxcb-render libxcb-shm libXcomposite libXcursor libXdamage libXdmcp libXext libXfixes libXi libXinerama libxkbcommon libxml2 libXrandr libXrender libxslt libyuv libz libzstd linux-vdso 
- GPUI: /lib64/ld-linux-x86-64 libasound libc libgcc_s libm libXau libxcb libxcb-xkb libXdmcp libxkbcommon libxkbcommon-x11 linux-vdso 

Installed size of the system web stack Tauri needs on this machine (context, not counted above):
- webkit2gtk-4.1: 141.18 MiB
- gtk3: 53.22 MiB
- libsoup3: 2.06 MiB

### Host: local

#### Startup, cold (fresh app state, no helper daemon)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Window mapped (ms) | 192 [182-223] | 163 [162-166] | 1.18x |
| First terminal content (ms) | 832 [800-857] | 230 [229-234] | 3.62x |
| CPU during startup window (CPU-s) | 1.20 [1.20-1.23] | 0.26 [0.25-0.27] | 4.62x |
| App PSS (max) (MB) | 496 [475-498] | 98 [98-99] | 5.05x |

#### Startup, warm (seeded state, daemon and tmux running)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Window mapped (ms) | 161 [159-195] | 116 [114-118] | 1.39x |
| First terminal content (ms) | 785 [700-786] | 157 [156-186] | 5.00x |
| CPU during startup window (CPU-s) | 1.21 [1.16-1.21] | 0.15 [0.15-0.16] | 8.07x |
| App PSS (max) (MB) | 491 [490-492] | 85 [85-85] | 5.78x |

#### Idle, 1 workspace x 1 terminal

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 3.5 [3.4-3.5] | 1.3 [1.3-1.4] | 2.61x |
| App GPU (all engines) (% busy) | 0.4 [0.4-0.4] | 0.5 [0.5-0.5] | 0.90x |
| Local host helper CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 1.00x |
| tmux CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Compositor CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.80x |
| Compositor GPU (% busy) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.73x |
| App PSS (mean) (MB) | 417 [416-419] | 85 [85-85] | 4.91x |
| App RSS (mean) (MB) | 603 [603-605] | 111 [110-111] | 5.45x |
| Host helper PSS (mean) (MB) | 12 [12-12] | 12 [12-12] | 1.00x |

#### Idle, 8 tabs x 2 panes

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 3.0 [3.0-3.2] | 1.4 [1.4-1.5] | 2.12x |
| App GPU (all engines) (% busy) | 0.4 [0.4-0.4] | 0.5 [0.5-0.6] | 0.78x |
| Local host helper CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 1.00x |
| tmux CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | 0.00x |
| Compositor CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 1.00x |
| Compositor GPU (% busy) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.82x |
| App PSS (mean) (MB) | 424 [423-427] | 96 [96-96] | 4.44x |
| App RSS (mean) (MB) | 613 [612-615] | 122 [121-122] | 5.04x |
| Host helper PSS (mean) (MB) | 12 [12-12] | 12 [12-12] | 0.98x |

#### Heavy output: seq 1 3000000

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Wall time to final line (s) | 2.25 [2.22-2.27] | 1.43 [1.43-1.45] | 1.58x |
| App CPU-seconds (CPU-s) | 4.17 [4.11-4.23] | 0.87 [0.86-0.92] | 4.79x |
| Local host helper CPU-seconds (CPU-s) | 0.47 [0.46-0.47] | 0.46 [0.45-0.47] | 1.02x |
| tmux CPU-seconds (CPU-s) | 1.45 [1.45-1.47] | 1.31 [1.30-1.34] | 1.11x |
| Compositor CPU-seconds (CPU-s) | 0.25 [0.23-0.25] | 0.17 [0.15-0.18] | 1.47x |
| App PSS (peak sample) (MB) | 740 [735-742] | 125 [124-125] | 5.94x |
| App RSS (peak sample) (MB) | 923 [919-925] | 150 [150-151] | 6.15x |

#### Heavy output: cat of 200k colored lines

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Wall time to final line (s) | 2.76 [2.69-2.78] | 2.44 [2.43-2.51] | 1.13x |
| App CPU-seconds (CPU-s) | 5.67 [5.57-5.68] | 1.67 [1.67-1.70] | 3.40x |
| Local host helper CPU-seconds (CPU-s) | 0.76 [0.76-0.77] | 0.89 [0.89-0.90] | 0.85x |
| tmux CPU-seconds (CPU-s) | 2.45 [2.43-2.45] | 2.35 [2.31-2.38] | 1.04x |
| Compositor CPU-seconds (CPU-s) | 0.29 [0.28-0.29] | 0.28 [0.28-0.29] | 1.04x |
| App PSS (peak sample) (MB) | 843 [842-870] | 132 [132-132] | 6.41x |
| App RSS (peak sample) (MB) | 1027 [1026-1054] | 157 [157-158] | 6.53x |

#### Agent-like line stream (60 lines/s, 8x2 layout)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 41.1 [40.7-41.1] | 20.6 [20.3-20.8] | 2.00x |
| App GPU (all engines) (% busy) | 4.7 [4.7-4.7] | 7.8 [7.8-7.8] | 0.60x |
| Local host helper CPU (% core) | 1.9 [1.9-1.9] | 1.9 [1.9-1.9] | 1.02x |
| tmux CPU (% core) | 1.2 [1.2-1.2] | 1.1 [1.1-1.1] | 1.11x |
| Compositor CPU (% core) | 2.5 [2.5-2.5] | 2.3 [2.3-2.3] | 1.09x |
| Compositor GPU (% busy) | 2.8 [2.8-2.8] | 2.8 [2.8-2.9] | 0.99x |
| App PSS (mean) (MB) | 459 [457-460] | 108 [108-108] | 4.25x |
| App RSS (mean) (MB) | 644 [642-646] | 134 [134-134] | 4.81x |
| Host helper PSS (mean) (MB) | 11 [11-11] | 12 [11-12] | 0.98x |

#### Recorded agents: 3 Claude + 3 Codex replays, rows working/done

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 21.9 [21.9-22.3] | 21.7 [21.6-21.7] | 1.01x |
| App GPU (all engines) (% busy) | 2.8 [2.7-2.8] | 7.3 [7.3-7.3] | 0.38x |
| Local host helper CPU (% core) | 1.5 [1.5-1.6] | 1.5 [1.4-1.5] | 1.03x |
| tmux CPU (% core) | 0.9 [0.9-0.9] | 0.9 [0.9-0.9] | 1.02x |
| Compositor CPU (% core) | 2.5 [2.4-2.5] | 2.5 [2.5-2.5] | 1.01x |
| Compositor GPU (% busy) | 2.9 [2.9-3.0] | 3.0 [3.0-3.0] | 0.98x |
| App PSS (mean) (MB) | 491 [490-494] | 104 [104-104] | 4.73x |
| App RSS (mean) (MB) | 680 [679-682] | 130 [130-130] | 5.23x |
| Host helper PSS (mean) (MB) | 13 [13-14] | 14 [14-14] | 0.96x |

#### 6 agents working, no output, window visible

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 10.3 [9.9-10.3] | 19.7 [19.5-19.7] | 0.52x |
| App GPU (all engines) (% busy) | 1.6 [1.6-1.7] | 7.3 [7.2-7.3] | 0.22x |
| Local host helper CPU (% core) | 0.2 [0.2-0.2] | 0.2 [0.2-0.2] | 1.00x |
| tmux CPU (% core) | 0.0 [0.0-0.1] | 0.1 [0.0-0.1] | 0.60x |
| Compositor CPU (% core) | 2.2 [2.2-2.2] | 2.5 [2.5-2.6] | 0.87x |
| Compositor GPU (% busy) | 2.8 [2.8-2.9] | 2.9 [2.9-2.9] | 0.97x |
| App PSS (mean) (MB) | 480 [480-483] | 104 [104-104] | 4.64x |
| App RSS (mean) (MB) | 666 [666-668] | 130 [130-130] | 5.13x |
| Host helper PSS (mean) (MB) | 13 [13-14] | 14 [13-14] | 0.95x |

#### 6 agents working, no output, window on another workspace

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 10.6 [10.2-10.6] | 1.0 [1.0-1.0] | 10.36x |
| App GPU (all engines) (% busy) | 1.7 [1.6-1.7] | 0.0 [0.0-0.0] | n/a (GPUI 0) |
| Local host helper CPU (% core) | 0.2 [0.1-0.2] | 0.2 [0.2-0.2] | 0.94x |
| tmux CPU (% core) | 0.0 [0.0-0.1] | 0.0 [0.0-0.1] | 1.00x |
| Compositor CPU (% core) | 0.7 [0.7-0.7] | 0.0 [0.0-0.0] | n/a (GPUI 0) |
| Compositor GPU (% busy) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| App PSS (mean) (MB) | 469 [468-470] | 104 [104-104] | 4.53x |
| App RSS (mean) (MB) | 655 [655-657] | 130 [130-130] | 5.04x |
| Host helper PSS (mean) (MB) | 13 [13-14] | 14 [13-14] | 0.95x |

#### Editor: Quick Open and open a 5 MB file (no scrolling)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Action window (s) | 8.6 [8.6-8.6] | 8.7 [8.6-8.7] | 1.00x |
| App CPU-seconds (CPU-s) | 1.68 [1.66-1.69] | 0.30 [0.30-0.30] | 5.60x |
| App GPU (all engines) (% busy) | 0.8 [0.7-0.8] | 1.6 [1.6-1.6] | 0.48x |
| Local host helper CPU-seconds (CPU-s) | 0.07 [0.07-0.08] | 0.07 [0.07-0.09] | 1.00x |
| Compositor CPU-seconds (CPU-s) | 0.07 [0.06-0.07] | 0.06 [0.05-0.07] | 1.17x |
| App PSS (peak sample) (MB) | 593 [587-597] | 141 [141-142] | 4.20x |
| App RSS (peak sample) (MB) | 793 [787-796] | 167 [167-167] | 4.75x |

#### Typing latency (keystroke to pixels)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Median latency (ms) | 30.6 [30.6-30.8] | 30.5 [30.4-30.5] | 1.00x |
| p95 latency (ms) | 47.4 [47.2-47.5] | 31.2 [31.2-31.6] | 1.52x |
| Min latency (ms) | 29.7 [29.6-29.7] | 22.9 [18.9-23.1] | 1.30x |
| Missed keys | 0 [0-0] | 0 [0-0] | = |

#### Compositor alone, no app window

Compositor CPU 0.03 % core, GPU 0.0 % busy over 60.0 s.

### Host: ssh

#### Startup, cold (fresh app state, no helper daemon)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Window mapped (ms) | 166 [162-207] | 201 [158-210] | 0.83x |
| First terminal content (ms) | 1450 [1394-1512] | 1096 [1093-1105] | 1.32x |
| CPU during startup window (CPU-s) | 1.36 [1.35-1.37] | 0.28 [0.28-0.29] | 4.86x |
| App PSS (max) (MB) | 472 [467-472] | 98 [98-98] | 4.84x |

#### Startup, warm (seeded state, daemon and tmux running)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Window mapped (ms) | 167 [162-168] | 112 [111-117] | 1.49x |
| First terminal content (ms) | 1294 [1174-1315] | 941 [925-950] | 1.37x |
| CPU during startup window (CPU-s) | 1.26 [1.26-1.26] | 0.21 [0.20-0.22] | 6.00x |
| App PSS (max) (MB) | 463 [452-467] | 86 [85-86] | 5.41x |

#### Idle, 1 workspace x 1 terminal

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 3.6 [3.5-3.6] | 1.4 [1.4-1.4] | 2.55x |
| App GPU (all engines) (% busy) | 0.4 [0.4-0.5] | 0.5 [0.5-0.5] | 0.92x |
| ssh client CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Compositor CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.80x |
| Compositor GPU (% busy) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.75x |
| App PSS (mean) (MB) | 418 [413-424] | 85 [85-86] | 4.90x |
| App RSS (mean) (MB) | 604 [600-610] | 111 [110-111] | 5.44x |
| Remote helper CPU (% core) | 0.3 [0.3-0.3] | 0.3 [0.3-0.3] | 1.00x |
| Remote tmux CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | 0.00x |
| Remote sshd CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Remote helper RSS (mean) (MB) | 23 [22-23] | 23 [23-23] | 0.99x |

#### Idle, 8 tabs x 2 panes

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 3.5 [3.5-3.5] | 1.5 [1.5-1.5] | 2.36x |
| App GPU (all engines) (% busy) | 0.4 [0.4-0.4] | 0.5 [0.5-0.5] | 0.76x |
| ssh client CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | 0.00x |
| Compositor CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.67x |
| Compositor GPU (% busy) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 0.75x |
| App PSS (mean) (MB) | 434 [431-436] | 96 [96-96] | 4.53x |
| App RSS (mean) (MB) | 623 [620-625] | 122 [121-122] | 5.13x |
| Remote helper CPU (% core) | 0.3 [0.3-0.4] | 0.3 [0.3-0.3] | 1.00x |
| Remote tmux CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | 1.00x |
| Remote sshd CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Remote helper RSS (mean) (MB) | 23 [23-23] | 23 [23-24] | 1.00x |

#### Heavy output: seq 1 3000000

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Wall time to final line (s) | 8.88 [8.88-8.93] | 8.78 [8.73-8.80] | 1.01x |
| App CPU-seconds (CPU-s) | 9.26 [8.59-9.77] | 2.31 [2.30-2.34] | 4.01x |
| ssh client CPU-seconds (CPU-s) | 0.33 [0.31-0.33] | 0.27 [0.26-0.27] | 1.22x |
| Compositor CPU-seconds (CPU-s) | 0.90 [0.89-0.95] | 0.87 [0.86-0.90] | 1.03x |
| App PSS (peak sample) (MB) | 548 [543-562] | 125 [124-125] | 4.40x |
| App RSS (peak sample) (MB) | 732 [727-746] | 150 [150-151] | 4.87x |
| Remote helper CPU-seconds (CPU-s) | 2.66 [2.59-2.78] | 2.46 [2.42-2.64] | 1.08x |
| Remote tmux CPU-seconds (CPU-s) | 8.68 [8.67-8.69] | 8.58 [8.56-8.65] | 1.01x |

#### Heavy output: cat of 200k colored lines

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Wall time to final line (s) | 14.06 [13.80-14.22] | 13.95 [13.71-14.11] | 1.01x |
| App CPU-seconds (CPU-s) | 18.95 [18.81-19.10] | 5.14 [5.13-5.29] | 3.69x |
| ssh client CPU-seconds (CPU-s) | 0.52 [0.51-0.53] | 0.55 [0.53-0.55] | 0.95x |
| Compositor CPU-seconds (CPU-s) | 1.42 [1.40-1.44] | 1.47 [1.45-1.50] | 0.97x |
| App PSS (peak sample) (MB) | 790 [789-800] | 131 [131-132] | 6.01x |
| App RSS (peak sample) (MB) | 974 [974-984] | 157 [157-157] | 6.19x |
| Remote helper CPU-seconds (CPU-s) | 4.91 [4.60-4.97] | 5.06 [4.92-5.16] | 0.97x |
| Remote tmux CPU-seconds (CPU-s) | 13.80 [13.53-13.94] | 13.76 [13.55-13.92] | 1.00x |

#### Agent-like line stream (60 lines/s, 8x2 layout)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 39.9 [39.0-40.9] | 20.0 [20.0-20.2] | 1.99x |
| App GPU (all engines) (% busy) | 4.8 [4.8-4.9] | 7.6 [7.6-7.6] | 0.63x |
| ssh client CPU (% core) | 0.7 [0.7-0.7] | 0.6 [0.6-0.6] | 1.20x |
| Compositor CPU (% core) | 2.5 [2.5-2.5] | 2.2 [2.2-2.3] | 1.12x |
| Compositor GPU (% busy) | 2.7 [2.7-2.8] | 2.8 [2.8-2.8] | 0.97x |
| App PSS (mean) (MB) | 462 [460-464] | 110 [110-111] | 4.19x |
| App RSS (mean) (MB) | 647 [645-650] | 136 [136-136] | 4.76x |
| Remote helper CPU (% core) | 6.3 [6.2-6.5] | 6.3 [6.3-6.4] | 1.01x |
| Remote tmux CPU (% core) | 2.9 [2.9-2.9] | 2.8 [2.8-2.8] | 1.02x |
| Remote sshd CPU (% core) | 2.2 [2.1-2.2] | 2.1 [2.1-2.1] | 1.01x |
| Remote helper RSS (mean) (MB) | 18 [18-19] | 19 [18-19] | 0.98x |

#### Recorded agents: 3 Claude + 3 Codex replays, rows working/done

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 22.5 [22.4-22.5] | 21.9 [21.6-22.0] | 1.03x |
| App GPU (all engines) (% busy) | 2.8 [2.7-2.8] | 7.3 [7.3-7.3] | 0.38x |
| ssh client CPU (% core) | 0.3 [0.3-0.3] | 0.3 [0.3-0.3] | 1.06x |
| Compositor CPU (% core) | 2.5 [2.4-2.5] | 2.5 [2.4-2.5] | 1.00x |
| Compositor GPU (% busy) | 3.0 [3.0-3.0] | 3.0 [3.0-3.0] | 1.00x |
| App PSS (mean) (MB) | 506 [500-507] | 104 [104-104] | 4.86x |
| App RSS (mean) (MB) | 694 [688-696] | 130 [130-130] | 5.33x |
| Remote helper CPU (% core) | 4.8 [4.8-4.8] | 4.9 [4.7-4.9] | 0.99x |
| Remote tmux CPU (% core) | 3.3 [3.3-3.3] | 3.3 [3.3-3.4] | 1.00x |
| Remote sshd CPU (% core) | 1.2 [1.2-1.2] | 1.2 [1.2-1.2] | 1.03x |
| Remote helper RSS (mean) (MB) | 25 [24-25] | 24 [24-24] | 1.01x |

#### 6 agents working, no output, window visible

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 10.4 [10.4-10.5] | 19.9 [19.6-20.1] | 0.52x |
| App GPU (all engines) (% busy) | 1.7 [1.7-1.7] | 7.3 [7.3-7.4] | 0.23x |
| ssh client CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Compositor CPU (% core) | 2.2 [2.2-2.3] | 2.5 [2.5-2.7] | 0.89x |
| Compositor GPU (% busy) | 2.9 [2.9-2.9] | 2.9 [2.9-2.9] | 0.99x |
| App PSS (mean) (MB) | 494 [488-497] | 105 [105-105] | 4.72x |
| App RSS (mean) (MB) | 681 [674-683] | 131 [131-131] | 5.20x |
| Remote helper CPU (% core) | 0.6 [0.5-0.6] | 0.6 [0.6-0.6] | 0.96x |
| Remote tmux CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 1.00x |
| Remote sshd CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Remote helper RSS (mean) (MB) | 21 [20-21] | 20 [20-20] | 1.01x |

#### 6 agents working, no output, window on another workspace

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| App CPU (% core) | 10.7 [10.3-10.9] | 1.1 [1.1-1.1] | 9.97x |
| App GPU (all engines) (% busy) | 1.7 [1.7-1.8] | 0.0 [0.0-0.0] | n/a (GPUI 0) |
| ssh client CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Compositor CPU (% core) | 0.7 [0.7-0.7] | 0.0 [0.0-0.0] | 35.00x |
| Compositor GPU (% busy) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| App PSS (mean) (MB) | 474 [474-477] | 105 [105-105] | 4.52x |
| App RSS (mean) (MB) | 660 [660-664] | 131 [131-131] | 5.04x |
| Remote helper CPU (% core) | 0.5 [0.5-0.5] | 0.5 [0.5-0.6] | 0.98x |
| Remote tmux CPU (% core) | 0.1 [0.1-0.1] | 0.1 [0.1-0.1] | 1.00x |
| Remote sshd CPU (% core) | 0.0 [0.0-0.0] | 0.0 [0.0-0.0] | = |
| Remote helper RSS (mean) (MB) | 21 [20-21] | 20 [20-20] | 1.01x |

#### Editor: Quick Open and open a 5 MB file (no scrolling)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Action window (s) | 8.7 [8.6-8.7] | 8.6 [8.6-8.6] | 1.00x |
| App CPU-seconds (CPU-s) | 1.69 [1.66-1.73] | 0.29 [0.28-0.30] | 5.83x |
| App GPU (all engines) (% busy) | 0.8 [0.8-0.8] | 1.6 [1.6-1.7] | 0.51x |
| Compositor CPU-seconds (CPU-s) | 0.08 [0.08-0.09] | 0.05 [0.05-0.06] | 1.60x |
| App PSS (peak sample) (MB) | 597 [573-620] | 141 [141-141] | 4.23x |
| App RSS (peak sample) (MB) | 797 [773-820] | 167 [166-167] | 4.78x |
| Remote helper CPU-seconds (CPU-s) | 0.21 [0.20-0.22] | 0.22 [0.21-0.24] | 0.95x |

#### Typing latency (keystroke to pixels)

| Metric | Tauri | GPUI | Tauri/GPUI |
|---|---|---|---|
| Median latency (ms) | 63.9 [63.8-64.2] | 63.9 [63.7-63.9] | 1.00x |
| p95 latency (ms) | 81.0 [80.7-81.1] | 66.5 [64.7-80.4] | 1.22x |
| Min latency (ms) | 63.1 [62.9-63.1] | 55.8 [54.1-62.7] | 1.13x |
| Missed keys | 0 [0-0] | 0 [0-0] | = |

