# Troubleshooting

Run `tmux-ide-host doctor` first. Use `doctor --json` when a machine-readable result is useful. If support needs a durable artifact, create one with:

```sh
tmux-ide-host support-bundle --output ./tmux-agent-ide-support.json
```

The command refuses to overwrite an existing path. Move or remove an old bundle, or select a different filename. Do not weaken its `0600` permissions.

Common report states:

- `daemon.state: not_running` — start or reconnect the desktop app, which starts the user daemon.
- `daemon.state: unsafe_or_invalid_endpoint` — a non-socket or non-private object occupies the daemon endpoint. Stop and inspect the user runtime directory rather than deleting an unknown object automatically.
- `daemon.runtimeState: invalid_or_unsafe` — runtime diagnostics had unsafe permissions, an unsupported/corrupt schema, or a symlink. Restarting the daemon creates a clean bounded state only when the runtime directory itself is private.
- A dependency marked `unavailable` or `failed` — install or repair that program and run the doctor again. Raw command errors are intentionally excluded; run the program's version command directly if you need its local detail.
- Nonzero connection or accept error counts — reconnect and check network/SSH/tmux availability. The report intentionally records only safe error classes, so local application logs may be needed for deeper investigation.

The report never includes terminal output, prompts, file contents, SSH configuration, credentials, hosts, users, or paths. See [Diagnostics, privacy, and security](diagnostics-privacy-security.md) before sharing it.
# macOS package and permissions

The internal macOS build is unsigned. If a quarantined artifact is blocked,
inspect it with `release/macos/verify-package.sh` and use the normal System
Settings privacy/security UI; do not disable Gatekeeper globally. Notification
denial is reported by the app and can be changed for `tmux Agent IDE` in System
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
  Notifications → tmux Agent IDE.
- *"This system has nothing to deliver notifications through"* — on macOS this
  means the running binary is not a bundle the system will register.
  `pnpm tauri dev` runs an unbundled binary, and a plain `tauri build` bundle can
  also be rejected when its signature seal is broken (M10-E016). **Only the
  output of `release/macos/build-package.sh` is expected to deliver
  notifications**; that script is what removes `LSRequiresCarbon` and re-signs
  the bundle. Verify a bundle with `release/macos/verify-package.sh`.

A notification for the pane you are currently looking at is suppressed on
purpose — the app is already showing that agent's state. Every other pane's
notification is shown, including while the app is frontmost.

The local helper uses a private runtime under
`~/Library/Caches/dev.dev.tmux-agent-ide/runtime` unless
`ADE_HOST_RUNTIME_DIR` is explicitly set. Remote Linux helpers are ELF files in
the application Resources directory; a macOS Mach-O helper is never uploaded
to Linux.
