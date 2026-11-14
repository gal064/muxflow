# Chat mode against Terminal mode: one benchmark

One run of measurements, on Linux, of the same agent in a terminal pane and
as a chat. Three cold starts per case, one real turn each ("Reply with
exactly: ok"), medians shown. `claude` 2.1.325 and `codex-cli` 0.169.0 on
their default models, a debug build of the helper, a 16-thread laptop with
other work running on it. Memory is PSS summed over the pane's process tree.

| | Claude terminal | Claude chat | Codex terminal | Codex chat |
| --- | --- | --- | --- | --- |
| Typing the name to ready | 0.34 s | 0.60 s | 0.05 s | 0.25 s |
| Idle memory | 224 MB | 200 MB | 417 MB | 244 MB |
| Peak memory in the turn | 258 MB | 214 MB | 426 MB | 239 MB |
| Idle memory after the turn | 252 MB | 214 MB | 429 MB | 225 MB |
| Idle CPU over 30 s | 0.18 s | 0.23 s | 0.01 s | 0.02 s |
| Send to first token | 1.16 s | 1.81 s | 2.45 s | 2.14 s |
| Send to turn end | 1.25 s | 1.88 s | 2.48 s | 2.63 s |

What it says:

- **A chat is not heavier than a terminal.** Claude headless is about 25 MB
  lighter than its terminal. One Codex chat is about 170 MB lighter than one
  Codex terminal, because the Codex terminal now starts an app-server of its
  own beside its interface.
- **Many Codex panes turn that round.** Codex terminals share one app-server,
  so a second terminal added about 68 MB; each Codex chat runs its own, and a
  second chat added about 112 MB. From roughly five Codex panes on, chats
  cost more memory than terminals. One server for all Codex chats would
  remove that.
- **Conversion costs about a quarter of a second** at start, for either
  agent.
- **Turn latency is not resolved.** It is model and network time, and the
  spread between runs (Claude terminal alone: 1.07 to 2.82 s) is wider than
  any difference between the modes. The two modes are also timed differently:
  the terminal by reading the screen, the chat by events on the wire, before
  the desktop draws them.

Not measured: a release build of the helper (its `chat-pipe` was 2 to 3 MB
here), the desktop app's own cost of drawing a chat against a terminal, a
long conversation, macOS.
