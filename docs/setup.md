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
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
pnpm check
pnpm test
pnpm build
```

Run a development build with `pnpm tauri dev`. tmux remains authoritative; the
application attaches to an existing server and closing the window detaches.

## Install the internal package

Build the package with `pnpm release:linux`. Extract the resulting archive from
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
# macOS

Development and the internal package require macOS 14 or newer, Xcode Command
Line Tools, Rust 1.97.1, Node 24, pnpm 11, tmux 3.3 or newer, and Git. Homebrew
installations under `/opt/homebrew` are supported from the non-interactive app
environment. The current packaged artifact supports Apple Silicon only.
