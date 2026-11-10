# Disposable S11 macOS instrument

These files belong to the spike, not the desktop or host engine. Read
`docs/gpui-s11-macos-report.md` for results, exclusions and remaining gates.
Run Python through `uv`.

`pixels.swift` is the common black-box observer: ScreenCaptureKit window capture,
CGEvent HID input, and AX window resizing. `run.py` drives both frontends and
uses native `footprint` process-group accounting. `paint.py` supplies a terminal
boundary sentinel; `flood.rs` is the spike's existing flood writer extracted for
both platforms. `control_probe.py` is a separate headless mechanism probe.

Build the meter **once**, before granting Accessibility and Screen Recording:

```sh
tests/performance/runtime/macos/build-meter.sh
```

Its bundle identity is `dev.muxflow.s11-meter`. Rebuilding an ad-hoc signed app
can invalidate grants. Normal runs never request permission; the explicit
`--request-permissions` mode does. `run.py` launches the meter through a private,
one-shot launchd job so the signed bundle keeps its identity.

The driver expects prepared disposable fixtures under `tmp/s11`:

- A packaged native Tauri app and native helper at
  `tmp/work/cache/release-target/macos/release/` (the package build output).
- `flood` for the Mac and `muxflow-host-linux-x86_64` built from this tree.
- `ssh_config`, a disposable key and known-hosts file. The only application SSH
  target is `muxflow-s11-container`, user `ade`, on a private address and port.
- An isolated container based on `tests/integration/transport/ssh-target`, with
  the Ubuntu 24.04 base (the built Linux helper needs glibc 2.39). Its published
  SSH port must bind only a private interface. Install the helper at
  `/home/ade/.local/bin/muxflow-host`, the Linux flood executable at
  `/usr/local/bin/s11-flood`, uv and uv-managed Python 3.12, and copy `paint.py`
  to `/home/ade/s11-paint.py` and `renderer_baseline.py` to `/home/ade/workload.py`.

The local fixture uses only runtime `/private/tmp/muxflow-s11-localhost`,
`TMUX_TMPDIR=/private/tmp/muxflow-s11-tmux` and socket `muxflow-qa-s11`.
The profile homes live under `tmp/s11/home-{app}-{endpoint}`. The private Tauri
state declines agent setup and maps tab 1/2 to Ctrl+Alt+F5/F6; it does not change
the user's app state. Each scenario replaces **only its fixture's** tmux server.
Never point this driver at a real user tmux server or host daemon.

With the Mac free for genuine input and both grants verified, run serially:

```sh
uv run --no-project python tests/performance/runtime/macos/run.py tauri ssh 1
uv run --no-project python tests/performance/runtime/macos/run.py gpui ssh 1
```

Repeat with run `2`, and endpoint `local` if tmux is installed. The default is
120 keys per ordinary typing scene. `S11_SCENARIOS` selects a comma-separated
subset; `S11_SAMPLES` is intended for instrument diagnostics. An advisory lock
refuses concurrent drivers. Do not run builds or other GUI tests during timings.
Results and raw `footprint` groups go under
`tests/performance/runtime/renderer-baseline/s11-macos`.

Afterward remove the exact disposable container, stop the private helper with
`daemon-stop --force --socket /private/tmp/muxflow-s11-localhost/host.sock`, stop
only the private tmux socket, and remove the `dev.muxflow.s11-meter-bench`
launchd job. Audit owned app/SSH children separately. Never use a process-name
kill or an unqualified daemon/tmux stop on the live Linux host.
