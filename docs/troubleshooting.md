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
