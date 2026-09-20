# Linux setup

## Supported environment

The validated Linux V1 package is x86-64 with GTK 3 and WebKitGTK 4.1. An
ARM64-native build lane is defined, but no retained native runner artifact or
ARM64 hardware result exists for this release, so ARM64 is not release-claimed.
The attached host may be local Linux or a remote Linux machine reachable
through OpenSSH. tmux 3.3 or newer is required; Git is optional until Git
features are used.

The application uses the system `ssh` executable and the user's existing SSH
configuration, agent, keys, `ProxyJump`, and known-hosts policy. It never imports
or stores SSH credentials.

## Build from source

Install the pinned Rust 1.97.1 toolchain, Node.js 24, pnpm 11.18, tmux, Git, and the Tauri 2
Linux prerequisites. On Debian/Ubuntu the GUI development packages include
`libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, and `librsvg2-dev`.

```sh
pnpm install --frozen-lockfile
cargo fmt --all -- --check
TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo clippy --workspace --all-targets -- -D warnings
TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo test --workspace --all-targets
pnpm check
pnpm test
pnpm build
```

Run a development build with `pnpm tauri dev`. tmux remains authoritative; the
application attaches to an existing server and closing the window detaches.

## Install the internal package

Build the package with `pnpm release:linux`. Docker is used to build the
remote helper against the Debian 12 compatibility baseline; the desktop itself
is built natively for the selected architecture. Extract the resulting archive from
`tmp/release`, then run its `install.sh`. The default rootless prefix is
`~/.local`; set `ADE_INSTALL_PREFIX` for a different location.

```sh
tar -xzf muxflow-*-linux-x86_64.tar.gz
cd muxflow-*-linux-x86_64
./install.sh
~/.local/bin/muxflow
```

The desktop and `muxflow-host` helper are installed as siblings. Profiles and
application state live outside the package directory and survive upgrades.

## First connection

For local use, start or retain any tmux server and select Local. For remote use,
create a saved SSH profile from a normal OpenSSH target such as `devbox` and,
when needed, an explicit SSH config path. The UI probes the remote architecture
and helper version before offering an explicit helper installation or upgrade.
It never requires root or systemd on the host.

Only one host/tmux server is active at a time in V1.

The app attaches to an existing tmux server and never owns its lifetime. For
remote use, first install the packaged helper through the reviewed profile flow;
the helper and agent hooks use SSH and a private Unix socket, not a TCP listener.
Uninstall refuses while managed hooks remain and preserves tmux sessions and
user configuration.

# macOS

Development and the internal package require macOS 14 or newer, Xcode Command
Line Tools, Rust 1.97.1, Node 24, pnpm 11, tmux 3.3 or newer, and Git. Homebrew
installations under `/opt/homebrew` are supported from the non-interactive app
environment. Muxflow searches the application environment, macOS path registry,
and the standard Homebrew, MacPorts, and pkgsrc prefixes without starting a
login shell. The current packaged artifact supports Apple Silicon only.

For a custom tmux installation, set `MUXFLOW_TMUX_PATH` to the absolute tmux
executable before starting the helper. An existing helper daemon retains the
environment it started with, so stop it with `muxflow-host daemon-stop` before
relaunching Muxflow after changing the override.

## Install the internal package

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

# Names and nested tmux

Workspace names are tmux session names and terminal-tab names are tmux window
names. Renaming either in the app updates tmux; renames made by another tmux
client appear live and survive reconnect. Pane titles remain owned by tmux and
the program running in the pane; V1 intentionally has no pane-title rename UI.
Unicode names require a UTF-8 locale on the host (a POSIX locale may normalize
non-ASCII characters), while names containing spaces are supported.

Nested tmux is unsupported. Running `tmux` inside a managed pane is ordinary
terminal content; the app does not discover, label, route, or control an inner
server. It is excluded from the ten Linux V1 release acceptance scenarios.
