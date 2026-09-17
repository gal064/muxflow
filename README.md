# Muxflow

A lightweight macOS/Linux desktop IDE that mirrors an existing tmux server. tmux remains authoritative for persistent sessions, windows, panes, and processes; the desktop adds graphical terminal, editor, Explorer, Git, transfer, and agent-attention surfaces.

The current build supports Linux and an ad-hoc-signed internal Apple-Silicon
macOS build. It includes local or single-host SSH tmux mirroring, xterm.js
terminals, Explorer/editor/Markdown, Git, Codex/Claude lifecycle attention,
native notifications, and bounded local/remote transfers. Intel Mac, signing,
and notarization are not claimed for the internal macOS artifact.

## Development

Requirements: Rust 1.97.1, Node.js 24, pnpm 11, tmux 3.3+, and the
[Tauri 2 platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
pnpm install --frozen-lockfile
pnpm test:transport:local
tmux new-session -s work
pnpm tauri dev
```

## Linux package

Docker is used to build the remote helper against the Debian 12 compatibility
baseline. The desktop itself is built natively for the selected architecture.

```sh
SOURCE_DATE_EPOCH=1704067200 pnpm release:linux -- x86_64
release/linux/verify-package.sh tmp/release/muxflow-*-linux-x86_64.tar.gz
tar -xzf tmp/release/muxflow-*-linux-x86_64.tar.gz -C /tmp
ADE_INSTALL_PREFIX="$HOME/.local" /tmp/muxflow-*/install.sh
muxflow
```

The app attaches to an existing tmux server and never owns its lifetime. For
remote use, first install the packaged helper through the reviewed profile flow;
the helper and agent hooks use SSH and a private Unix socket, not a TCP listener.
Uninstall refuses while managed hooks remain and preserves tmux sessions and
user configuration.

## Internal macOS package

The macOS build is currently Apple Silicon only and uses an ad-hoc code identity
so native macOS services can identify the app. It has no Developer ID signature,
Gatekeeper trust, or notarization claim. It
packages a native Mach-O local helper plus separate Linux ELF helpers for ARM64
and x86_64 SSH targets.

```sh
pnpm release:macos
release/macos/install.sh
open /Applications/Muxflow.app
release/macos/uninstall.sh
```

`install.sh` defaults to the bundle just built under `tmp/work` and takes an
absolute path to any other `Muxflow.app`. The installer verifies ownership
before upgrades, rolls back a failed publication, and preserves tmux sessions
and application configuration. It installs to `/Applications`, which requires
an account that can write there; set
`ADE_MACOS_APPLICATIONS_DIR="$HOME/Applications"` for a rootless per-user
install. If a copy is left in the other location the installer names both
paths so it can be removed deliberately.

Workspace names are tmux session names and terminal-tab names are tmux window
names. Renaming either in the app updates tmux; renames made by another tmux
client appear live and survive reconnect. Pane titles remain owned by tmux and
the program running in the pane; V1 intentionally has no pane-title rename UI.
Unicode names require a UTF-8 locale on the host (a POSIX locale may normalize
non-ASCII characters), while names containing spaces are supported.

Nested tmux is unsupported. Running `tmux` inside a managed pane is ordinary
terminal content; the app does not discover, label, route, or control an inner
server. It is excluded from the ten Linux V1 release acceptance scenarios.

See [setup](./docs/setup.md), [SSH setup](./docs/remote-host.md),
[release instructions](./docs/release.md), [troubleshooting](./docs/troubleshooting.md),
the [test suite guide](./tests/README.md), and the
[historical plans](./docs/history/README.md).

## License

Muxflow source is licensed under [MIT](./LICENSE). The bundled JetBrains Mono
fonts retain their own license in the desktop and mobile font directories. The
optional speech model is downloaded separately; its upstream and conversion
model cards are linked in [third-party notices](./docs/third-party.md).
