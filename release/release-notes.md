## GPUI desktop release candidate

The desktop app now uses GPUI. Existing profiles, settings and install locations
are retained. Linux needs a working Vulkan or OpenGL graphics driver.

Live Markdown table editing keeps the caret in its cell after whitespace edits.
Enter adds a line inside a cell; Tab moves between cells. Packages include
third-party licence notices, and the unused KDE blur dependency is removed.
Routine success toasts for closing tabs and other visible changes are suppressed;
download results and failure notices remain.

This draft candidate has automated test and package checks. Manual QA was
skipped: Mac upgrade and Live Markdown checks, minimized CPU measurements, and
physical trackpad/touchpad checks remain unverified. It is not a stable release.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
