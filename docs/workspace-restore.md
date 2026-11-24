# Workspace restore: the host recreates its tmux sessions after the server dies

Status: implemented.

## Summary

A tmux server does not survive a reboot, a crash, or a `kill-server`, and
everything inside it — sessions, windows, the agents running in them — is
gone. Workspace restore is the host remembering the sessions it has seen,
with the directory of every window, and recreating them when the server comes
back. It lives on the host, not on any client, for two reasons:

- The record outlives every client. A desktop only sees what happened while
  it was connected; the host watches sessions close and knows the difference
  between "the user killed it" and "the server died".
- One implementation serves the phone and every desktop.

Off until asked, per host. While off the host remembers nothing, so a person
who turned it off can trust that no paths of theirs are kept.

## Using it

Switch it on in **Settings → Workspace → Reopen after a tmux loss**, for the
host on screen. The switch belongs to the host — the same answer shows from
every device that asks — and enabling starts the record from what is open
then. The row also shows what the host currently remembers.

## How it works

The host's `workspace_restore` plugin looks at the live view every 15 seconds
(one `list-panes -a`) and keeps a record in the state directory:

- Linux: `~/.local/state/muxflow/workspace-restore.json`
- macOS: `~/Library/Application Support/dev.muxflow.desktop/workspace-restore.json`

The record holds the sessions and, per window, the directory of its first
pane. Window names are deliberately not stored: recreating one would take
`new-window -n`, which switches tmux's automatic renaming off for that window
forever.

The server's identity — the dev:ino of its socket, the same string the
topology publishes — decides everything:

- **Same identity, changed sessions.** The live view is the whole truth. A
  session the user closed leaves the record here; it stays closed.
- **A different identity.** The old server is gone and another is here (tmux
  cannot hand a new server an old socket with old inode numbers). Every
  remembered session that is not already open is recreated: `new-session -c`
  for its first window, `new-window -c` for the rest, each at the directory
  it was last in. A directory that no longer exists starts that window at
  tmux's own choice instead — the session coming back is worth more than the
  exact starting point. The new server's live view becomes the record.
- **No server at all.** The record holds the layout the last server had, and
  the loss is noted, so the next server is a new one even if it reuses the
  socket path.

Everything is capped and re-checked on read (session and window counts, name
and directory shape), and a record written by a future version is refused
rather than half-trusted.

## Wire

`OPERATION_WORKSPACE_RESTORE` (`workspace_restore.proto`) with two actions:
`GET` reads the switch and the remembered layout, `SET_ENABLED` moves the
switch and answers with the layout that follows. The desktop Settings is the
only caller; there are no events, and older clients never send the operation,
so it is additive and `PROTOCOL_MAJOR` stays.

## Removal

The host half is `apps/host/src/service/plugins/workspace_restore/` and its
lines in `plugins/mod.rs`; the desktop half is `apps/desktop-gpui/src/
host_restore/`, its Workspace-tab section, and its lines in `connection_ui/
mod.rs` and `connection_ui/backend.rs`. The proto file is separate;
`envelope.proto` names it in three places.
