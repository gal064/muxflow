# Troubleshooting

Run `muxflow-host doctor` first. Use `doctor --json` when a machine-readable result is useful. If support needs a durable artifact, create one with:

```sh
muxflow-host support-bundle --output ./muxflow-support.json
```

The command refuses to overwrite an existing path. Move or remove an old bundle, or select a different filename. Do not weaken its `0600` permissions.

Common report states:

- `dependencies.tmux.available: false` — install tmux 3.3 or newer. Muxflow
  automatically checks standard system, Homebrew, MacPorts, pkgsrc, Linuxbrew,
  and Nix locations. For another layout, set `MUXFLOW_TMUX_PATH` to the absolute
  executable path and restart the helper daemon.
- `daemon.state: not_running` — start or reconnect the desktop app, which starts the user daemon.
- `daemon.state: unsafe_or_invalid_endpoint` — a non-socket or non-private object occupies the daemon endpoint. Stop and inspect `/tmp/muxflow-<uid>` rather than deleting an unknown object automatically.
- `daemon.runtimeState: invalid_or_unsafe` — runtime diagnostics had unsafe permissions, an unsupported/corrupt schema, or a symlink. Restarting the daemon creates a clean bounded state only when the runtime directory itself is private.
- A dependency marked `unavailable` or `failed` — install or repair that program and run the doctor again. Raw command errors are intentionally excluded; run the program's version command directly if you need its local detail.
- Nonzero connection or accept error counts — reconnect and check network/SSH/tmux availability. The report intentionally records only safe error classes, so local application logs may be needed for deeper investigation.

The report never includes terminal output, prompts, file contents, SSH configuration, credentials, hosts, users, or paths. See [Diagnostics, privacy, and security](diagnostics-privacy-security.md) before sharing it.

# Codex agents show "unknown"

Codex 0.157 and newer connect every `codex` window to a shared background
server (`codex app-server --managed-daemon`). That server runs hooks with its
own environment, captured when it started, so a hook cannot tell which tmux
pane it came from and Muxflow drops the event. The agent stays `unknown` and
never notifies.

When you set up a host, Muxflow makes two changes, both re-applied on every
connect and removed when you uninstall the agent hooks:

- **The fix:** the tmux server's global environment gets an empty
  `CODEX_EXEC_SERVER_URL`. Codex skips the background server whenever that
  variable is set, even to an empty value, and runs everything else as usual.
  This works even while a background server is running. It lives in the tmux
  server's memory, not in `~/.tmux.conf`.
- **A setting:** `~/.codex/config.toml` gets `daemon_auto_start = false` under
  `[features]`, marked with a Muxflow comment. Without it, Codex warns on every
  launch that it is running without the background server. It applies to all of
  Codex on the host, so Codex started outside tmux no longer starts the server on
  its own either. A value you set yourself is never changed, and a symlinked or
  unreadable `config.toml` is left alone.

If Codex still shows `unknown`:

1. Check the variable: `tmux show-environment -g CODEX_EXEC_SERVER_URL` should
   print `CODEX_EXEC_SERVER_URL=`. If it prints a value you set, Muxflow keeps
   yours and Codex status can't work.
2. Start Codex in a new pane or tab. A shell that was already open before
   Muxflow connected started without the variable.
3. Optionally stop the background server with `codex app-server daemon stop`.
   It can leave its `codex app-server daemon pid-update-loop` updater running
   ([openai/codex#48195](https://github.com/openai/codex/issues/48195)); stop that
   process too.

This relies on how Codex reads the variable today. The proper fix is for Codex
to pass the window's environment to hooks
([openai/codex#44902](https://github.com/openai/codex/issues/44902)).

# Terminal keys

## Ctrl+/ and Ctrl+_

Both deliver the same byte, `0x1f` (ASCII US), and a TUI cannot tell them
apart. That is a property of the terminal encoding, not of Muxflow: Ctrl+`_`
masks to `0x1f` directly, and terminals have long aliased Ctrl+/ onto the same
byte. So a program that binds Ctrl+/ (undo in Emacs, comment-toggle in several
editors) and a program that binds Ctrl+_ are binding the same input.

Muxflow sends `0x1f` for Ctrl+/ explicitly, because xterm.js has no mapping for
that key and would otherwise send nothing at all. Ctrl+_ is left to xterm.js,
which already encodes it correctly. Ctrl+Shift+/ (Ctrl+?) is a different key and
is unaffected.

# macOS package and permissions

The macOS build is ad-hoc signed and not notarized, so Gatekeeper blocks a
quarantined copy on first launch. Allow it once with System Settings → Privacy
& Security → Open Anyway (or `xattr -dr com.apple.quarantine` on the app)
rather than disabling Gatekeeper globally; `release/macos/verify-package.sh`
inspects a bundle. Notification
denial is reported by the app and can be changed for `Muxflow` in System
Settings. Accessibility and Screen Recording are required only by the QA
driver, not by normal app operation.

## Notifications never appear on macOS

Settings → Sounds has a **Send test notification** button and a line saying what
macOS currently permits. Read that line first; it separates the three reasons a
notification does not arrive.

- *"Not requested yet"* — nothing has ever asked. Permission is requested lazily
  on the first agent event, so on a machine where no agent has blocked or
  finished, the app never appears in System Settings at all. Pressing the button
  is what raises the prompt.
- *"Notifications are turned off for this app"* — grant them in System Settings →
  Notifications → Muxflow.
- *"macOS did not answer"* — the running binary is not a bundle the system will
  register, so the permission query never comes back. There is no framework
  status for this; the silence *is* the symptom. `pnpm tauri dev` runs an
  unbundled binary, and a plain `tauri build` bundle can also be rejected when
  its signature seal is broken (M10-E016). **Only the output of
  `release/macos/build-package.sh` is expected to deliver notifications**; that
  script is what removes `LSRequiresCarbon` and re-signs the bundle. Verify a
  bundle with `release/macos/verify-package.sh`.
- *"…did not report a notification permission this app understands"* — on Linux,
  no notification daemon is answering on the session bus. On macOS, a permission
  state newer than this build.

A notification for the pane you are currently looking at is suppressed on
purpose — the app is already showing that agent's state. Every other pane's
notification is shown, including while the app is frontmost.

**Notifications arrive but the test one is silent.** macOS fixes an app's
notification options at the first authorization request and never asks again.
Builds before this one asked for alerts only, so an install that granted
permission then has no sound permission now and cannot be re-prompted from
inside the app. Turn sound on in System Settings → Notifications → Muxflow,
or revoke and re-grant.

The local helper executable remains in the application bundle. Its private
communication socket is `/tmp/muxflow-<uid>/host.sock` unless
`ADE_HOST_RUNTIME_DIR` is explicitly set; durable macOS helper state is under
`~/Library/Application Support/dev.muxflow.desktop`. Remote Linux helpers are
ELF files in the application Resources directory; a macOS Mach-O helper is
never uploaded to Linux.
