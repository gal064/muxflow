## Install / upgrade

tmux 3.3 or newer must be installed on every host you attach to.

### Linux (x86_64 and aarch64)

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/latest/download/install.sh | bash
```

To upgrade, quit Muxflow, run the same command again, and relaunch. It
installs under `~/.local` and needs GTK 3 and WebKitGTK 4.1 (Arch:
`webkit2gtk-4.1`; Debian/Ubuntu: `libwebkit2gtk-4.1-0`). The `.tar.gz` files
below are for a manual install; most people should use the command above.

### macOS (Apple Silicon)

Download `Muxflow_{{VERSION}}_aarch64.dmg` below, quit Muxflow if it is
running, drag `Muxflow.app` to `/Applications` (replacing the old copy), and
open it. The app
is not notarized by Apple, so macOS blocks the first launch. To allow it, open
System Settings → Privacy & Security and choose **Open Anyway**, or run
`xattr -dr com.apple.quarantine /Applications/Muxflow.app`.

### Android

Download `muxflow-{{VERSION}}-android.apk` below and sideload it.

