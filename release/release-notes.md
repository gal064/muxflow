## New desktop app

The desktop app is now native, built with GPUI instead of a web view. It
installs over the previous version in place: saved hosts, settings and
workspaces carry over, and hosts and phones need no change.

On Linux it draws with the GPU through Vulkan, so it needs the Vulkan loader and
a driver (`libvulkan1` and `mesa-vulkan-drivers` on Debian and Ubuntu). The
installer checks for the loader. If the window does not open, see
[troubleshooting](https://github.com/gal064/muxflow/blob/main/docs/troubleshooting.md#the-window-does-not-open-on-linux).

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
