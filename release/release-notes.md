## GPUI desktop

The desktop app now uses GPUI. Existing profiles, settings and install locations
are retained. Linux needs a working Vulkan or OpenGL graphics driver.

Live Markdown table editing keeps the caret in its cell after whitespace edits.
Enter adds a line inside a cell; Tab moves between cells. Packages include
third-party licence notices, and the unused KDE blur dependency is removed.
Routine success toasts for closing tabs and other visible changes are suppressed;
download results and failure notices remain.

Phones need the 0.1.33 app or newer: 0.1.33 changed the host's protocol, so
hosts from 0.1.33 on refuse older phone apps and older hosts refuse the new app.
Update the Android app from the APK below; the iOS build is on TestFlight.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
