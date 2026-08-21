---
name: test-desktop-with-cua
description: Build and drive the packaged desktop app on Linux or macOS with CUA.
---

# Test the packaged desktop

1. Build the real package: `pnpm release:linux -- x86_64` on Linux or `pnpm release:macos` on macOS.
2. Start an isolated app home plus disposable Docker/SSH target. On Linux, run `ADE_PHASE8_CUA_ARCHIVE=<archive> tests/release/setup-cua.sh`; give macOS an equivalent fixture around the packaged `.app`.
3. Use `cua call start_session ...`, `cua call list_windows ...`, and `cua call get_window_state ...` to drive the app. Verify effects independently over SSH.
4. Force an SSH disconnect, confirm the UI fails closed, reconnect, and verify another command reaches the same tmux session.
5. End the CUA session and remove the app, tmux server, helper daemon, and container.
