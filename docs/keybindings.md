# Key bindings

Every keyboard shortcut and mouse gesture in the Muxflow desktop app.

The Linux keymap is the macOS keymap with ⌘ read as Ctrl. Where that would take
a key the terminal depends on (Ctrl+D is end-of-file, Ctrl+E is end-of-line) or
collide with another binding, Linux uses Ghostty's default instead.

Any command in the tables can be rebound or unbound: open the command palette
and run **Configure keyboard shortcuts**. Commands without a default key are
still in the palette and can be given one there.

## Application

| Command | macOS | Linux |
|---|---|---|
| Command palette | ⌘K | Ctrl+K |
| Quick Open (files, workspaces, tabs) | ⌘P | Ctrl+P |
| Settings | ⌘, | Ctrl+, |
| Toggle sidebar | ⌘B | Ctrl+B |
| Toggle right panel | ⌘L | Ctrl+L |
| Show Files | ⌘⇧E | Ctrl+Shift+E |
| Show Source Control | ⌘⇧G | Ctrl+Shift+G |
| Back | ⌘[ | Ctrl+[ |
| Forward | ⌘] | Ctrl+] |
| Jump to the agent that needs you | ⌘⇧U | Ctrl+Shift+U |

## Workspaces and tabs

Quick Open matches file names and paths beneath the focused terminal pane's
working directory, tabs in the current workspace, and workspace names, paths,
and branches across shown hosts. Enter opens a file or switches to the selected
tab or workspace. An already-open file appears as a tab result. File contents
are not searched. The directory and host are shown below the results.

Opening Quick Open with an empty query lists tabs and workspaces immediately.
File searches start only after typing; a limited search asks you to narrow the
query. Workspaces and tabs remain available if file search fails.

| Command | macOS | Linux |
|---|---|---|
| New workspace | ⌘N | Ctrl+N |
| Go to workspace 1–9 | ⌘1–9 | Ctrl+1–9 |
| New terminal tab | ⌘T | Ctrl+T |
| Close current tab | ⌘W | Ctrl+W |
| Previous tab | ⌘⇧[ | Ctrl+Shift+[ |
| Next tab | ⌘⇧] | Ctrl+Shift+] |
| Go to tab 1–9 | ⌃1–9 | Alt+1–9 |

## Panes

| Command | macOS | Linux |
|---|---|---|
| Split right | ⌘D | Ctrl+Shift+O |
| Split down | ⌘⇧D | Ctrl+Shift+D |
| Focus pane left / right / up / down | ⌘⌥ + arrow | Ctrl+Alt + arrow |
| Resize pane | — | Ctrl+Shift + arrow |
| Toggle pane zoom | ⌘E | Ctrl+Shift+Enter |

## Terminal

| Command | macOS | Linux |
|---|---|---|
| Copy selection | ⌘C | Ctrl+C (see below), Ctrl+Shift+C |
| Paste | ⌘V | Ctrl+V, Ctrl+Shift+V |
| Find in terminal | ⌘F | Ctrl+F |
| Line start / end | ⌘← / ⌘→ | — |
| Newline in an agent prompt (Claude Code, Codex) | ⇧Enter | Shift+Enter |

**Ctrl+C on Linux** copies only while the pane has text selected, and clears
the selection as it copies. With nothing selected it reaches the shell as the
interrupt, so pressing Ctrl+C twice copies and then interrupts. A selection
left by Find doesn't count. Ctrl+Shift+C and Ctrl+Shift+V always copy and
paste.

On Linux Muxflow takes several Ctrl keys that shells also use, including Ctrl+W
(delete word), Ctrl+V (insert next key literally), Ctrl+B, Ctrl+K, Ctrl+L,
Ctrl+P, Ctrl+N, Ctrl+T, Ctrl+F and Ctrl+[ (Escape). If you need one of them in
the terminal, unbind or rebind the command in **Configure keyboard shortcuts**.

On Omarchy, the system-wide Super+C and Super+V also copy and paste in Muxflow.

## Mouse

| Gesture | Where | What it does |
|---|---|---|
| Shift-click | Workspace in the sidebar | Pin it to the top of the list, or unpin it. Doesn't switch to it. |
| Shift-click | Terminal tab | Pin it to the front of the strip, or unpin it. Doesn't select it. |
| Shift-click | Agent in the sidebar | Pin the agent's tab, or unpin it. Doesn't navigate to it. |
| Double-click | Workspace in the sidebar | Rename it |
| Double-click | Terminal tab | Rename it |
| Double-click | Preview document tab | Keep it open as a regular tab |
| Middle-click | Tab | Close it |
| Right-click | Workspace, tab, agent, file or change | Open its menu (the Menu key or Shift+F10 does the same from the keyboard) |
| ⌘-click / Ctrl-click | Link in the terminal | Open it (anywhere else in the pane, the same press grabs the pane; see below) |
| Right-click | Terminal | Select the word under the pointer |
| Shift-drag | Terminal | Select text even when the program running there uses the mouse (Option-drag works too on macOS) |
| Drag | Title bar | Move the window (Linux, and the empty part of the bar on macOS) |
| Drag | Tab | Reorder the tabs. Drop it on a workspace in the sidebar to move it there. A terminal tab with a single pane can also be dropped on the edge of a pane to become a split beside it. |
| Drag | Agent in the sidebar | Drop it on the edge of a pane to split it in beside that pane, on the tab strip to give it a tab of its own, or on a workspace to move it there |
| ⌘-drag / Ctrl-drag | Terminal pane | Drop it on the edge of another pane to move it there (within the same tab, this rearranges the panes: side by side, stacked, or reordered), on the tab strip to make it a tab of its own, or on a workspace to move it there |
| Escape | While dragging | Cancel the drag |

To show only pinned workspaces, run **Show pinned workspaces only** from the
command palette.

Moving a tab, agent or pane keeps you where you are: the workspace, tab and
pane on screen stay on screen. When the thing you move is the pane you're
looking at, the view goes with it to its new tab or workspace. A move that
would leave a workspace with nothing in it is refused, since that would close
the workspace.

On Linux the Ctrl-press that grabs a pane is not passed to the terminal, so a
program that reads the mouse (vim's Ctrl-click, for one) no longer sees a
Ctrl-click there. On a link it still opens the link.

An agent's menu (right-click it in the sidebar) also has **Bring here as
split**, which places the agent's pane beside the pane you're in (for an agent
in another tab), and **Move to workspace…**.

**Focus pane on hover** in Settings focuses a split pane when the pointer rests
on it, without a click.

**Copy on select** in Settings copies terminal text as soon as you finish
selecting it.

## Lists, menus and dialogs

| Key | Where | What it does |
|---|---|---|
| ↑ / ↓, Enter | Command palette, Quick Open | Move, then run or open |
| ← / →, Home / End | Focused tab strip | Move between tabs |
| ↑ / ↓ | Focused workspace or agent list | Move between rows |
| ↑ / ↓, Home / End, Tab, Escape | Open menu | Move, or close the menu |
| Escape | Dialog | Cancel |
| Enter | Find bar | Next match |
| ⌘Enter / Ctrl+Enter | Commit message | Commit |
| ← / → | Focused sidebar edge | Resize the sidebar |
| ↑ / ↓ | Focused divider above the agent list | Resize the agent list |
