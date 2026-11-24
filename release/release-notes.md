## Codex chat catches up with Claude Code

Codex chats now run `/compact`, `/review` and `/init`. `/review` reviews
uncommitted changes; follow it with a branch, a commit or your own
instructions to review those instead. A command typed while Codex is working
waits for the turn to end.

Codex subagents show what they are doing, how many tools they have used and
how many tokens they have spent, and get a pill above the composer like
Claude's. A subagent's row keeps running after the turn that started it ends.
Codex's plan is one row that updates as it goes. Warnings, retries and model
switches are shown in the chat, along with image views, waits and generated
images.

## Smaller changes

- The background chip above the composer sits on the left, for Claude and
  Codex alike.
- The sidebar's "all" and agent-ordering toggles are grey, without the
  chevron.
- A link in a chat opens with Cmd-click (Ctrl-click on Linux) as well as a
  plain click, as in a terminal. Cmd-dragging still moves the pane.
- Panes no longer draw a ring around the active pane or the one that needs
  you.
- The chat no longer jumps while long Markdown replies finish rendering.
- Show in Finder after a download no longer crashes the app on macOS.

Phones need the 0.1.33 app or newer, as before; the protocol is unchanged.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
