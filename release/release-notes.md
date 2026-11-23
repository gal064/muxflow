## Stage all, discard all and Commit & Push

The desktop's Source Control panel works more like VS Code's. Unstaged and new
files share one Changes list, new files marked U, above Staged changes. Hover a
group's heading to stage, unstage or discard the whole group; the command
palette has the same three. Discard all asks once, reverts the listed files,
deletes the new ones and keeps what is staged. None of them touches a merge
conflict.

Commit commits what is staged, or, when nothing is staged, every change, new
files included; its label says which. Commit & Push and Push sit under it. An
empty message commits as "no commit message". Committing waits until merge
conflicts are resolved.

On the phone, the terminal uses the colors the desktop gives programs, so
Codex's prompts stay readable when the desktop uses a light theme.

Phones need the 0.1.33 app or newer, as before; this release adds to the
protocol without changing its version. Update the Android app from the APK
below; the iOS build is on TestFlight.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
