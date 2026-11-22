## Quick sessions and shortcuts

Settings › Keyboard lets you change the desktop's shortcuts. Another setting
chooses whether ⌘1–9 (Ctrl+1–9 on Linux) jumps to workspaces or to agents;
with agents, holding ⌘ (Ctrl on Linux) shows each agent's number. ⌘⇧N (Ctrl+Shift+N on Linux)
starts the host's agent command in a workspace named "random", creating it if
needed. The agent command is set per host in the desktop.

The phone app has a New agent | New terminal bar at the bottom of the Agents and
Workspaces tabs. Both open in "random", in the host's Default directory, which
you set in Settings.

The iOS app is now built with the iOS 27 SDK on Expo 58 and React Native 0.88.
Phones need the 0.1.33 app or newer, as before; this release does not change the
protocol. Update the Android app from the APK below; the iOS build is on
TestFlight.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
