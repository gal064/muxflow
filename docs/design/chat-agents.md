# Chat mode and T064 — subagent plan

How the work in [chat-plan.md](chat-plan.md) is split across agents. Root
is the session that dispatches; it owns the result. The ground rules, stop
rules and machine limits in
[gpui-migration-agents.md](../gpui-migration-agents.md) apply unchanged, with
the differences below.

## Differences from the migration rules

- **Base:** `gpui/chat`, in `../muxflow-chat`, cut from `gpui/integration`.
- **Task worktrees:** `../muxflow-chat-<task>` on branch `gpui/chat-<task>`,
  cut from `gpui/chat` at the start of the task's wave. Root creates them.
  Removed after merge, with `target` deleted.
- **Wire changes are planned here.** Only task A changes the shared messages and
  bumps `PROTOCOL_MAJOR`. Task D adds its Git messages. Any other task that
  needs a wire change stops and reports it.
- **Nothing is pushed** and nothing merges outside `gpui/chat` without the
  owner saying so.
- **Privacy:** the repository is public. No machine names, user names, home
  paths or raw measurement output in commits.

## Isolation for every task

- `~/dev/muxflow-gpui-tools/locked` first on `PATH`: one cargo command at a time
  across all agents. Run built binaries directly, never through `cargo run`.
- Its own tmux socket (`tmux -L muxflow-qa-<task>`), its own `ADE_HOST_RUNTIME_DIR`
  and a private `HOME` for anything that starts the helper or the app. Never
  touch the owner's live daemon, tmux server, `~/.claude` or `~/.codex`.
- Anything that launches the app runs headless inside `qa-sandbox.sh`, as the
  migration rules describe. No window opens on the owner's desktop.
- Kill every daemon, tmux server and agent process the task started.
- **Real agent runs cost the owner's subscription.** Only task B and the final
  QA may run a real `claude` or `codex` turn. Use the cheapest model and
  one-line prompts, and stay under 20 turns per provider per task.

## Tasks

All six are **full cycle**: the worker loads `gdev_cycle`
(`~/.claude/skills/gdev_cycle/SKILL.md`), runs autonomously, and owns its
implementation, reviews, QA agent and commit. Root does not repeat a worker's
reviews. "Invariant" and "Design" name the extra reviewers `gdev_cycle` asks
for.

| Task | Scope (plan §5) | Size, reviewers | Model, effort | Lightweight tests | Worker QA (`gdev_qa_<task>`) |
| --- | --- | --- | --- | --- | --- |
| **A** contract | Stream A | Large. Code + invariant | Fable, high | `cargo test -p tmux-agent-protocol -p muxflow-host -p desktop-core`, clippy, fmt, `cargo deny check licenses`, `pnpm mobile:check`, mobile `protocol:check` | Start a private daemon with the scripted provider and drive OPEN, SEND, ANSWER, REWIND and LOAD_OLDER through `bridge --stdio`; confirm event order and revisions |
| **B** host chat | Stream B | Large. Code + invariant | Fable, high | `cargo test -p muxflow-host`, clippy, fmt | On a private tmux socket: type `claude`, then `codex`, in Chat mode; each converts, runs one turn, one question, one interrupt and one rewind; restart the daemon mid-turn and confirm reconnect; confirm the three open provider facts |
| **C** desktop chat | Stream C | Large. Code + design | Fable, high | `cargo test -p muxflow-gpui` on `chat`, clippy, fmt | Headless app against a private daemon with the scripted provider; capture a frame of every state in the mock and compare |
| **D1** Git view | Stream D without the card | Medium. Code + design | Opus, high | `cargo test -p muxflow-host -p muxflow-gpui` on `git`, clippy, fmt, mobile `protocol:check` | Headless app on a scratch repository: hunks-only diff in one reused tab, both scopes, stage and discard one hunk and check `git diff` agrees |
| **D2** details card | The card | Small. Code + design | Opus, medium | `cargo test -p muxflow-gpui` on `chat` | Headless frames at a wide and a narrow chat width; branch menu and Changes open the right thing |
| **E** History, Snooze | Stream E | Medium. Code | Opus, high | `cargo test -p muxflow-host -p muxflow-gpui` on `history` and `agents`, clippy, fmt | On a private tmux socket: a closed session appears in History and resumes into the right workspace; a one-minute snooze wakes as a placeholder across a daemon restart |
| **F** phone | Stream F | Large. Code + design | Opus, high | `pnpm mobile:check`, `pnpm mobile:test` | Component tests rendering each phone mock state from scripted events. No device run (see "Phone") |

## File overlap and how it is handled

| Files | Tasks | Handling |
| --- | --- | --- |
| `crates/protocol/proto/*`, generated mobile protocol | A, then D1 | A merges before wave 1. D1 touches only Git messages and regenerates. |
| `apps/host/src/service/requests/*` | A, D1 | A adds chat and history arms; D1 only extends the Git handler. |
| `apps/host/src/service/agents/{mod,ingest,reconcile}.rs`, `hook.rs` | B, E | B adds the conversion trigger and keeps the hook `source` field; E adds the history feed. Each adds one call into its own module. Merge B, then E. |
| `apps/host/src/service/chat/*` | A (skeleton), B | B builds on A's files after A is merged. |
| `apps/desktop-gpui/src/terminal/surface.rs` | C | C alone. |
| `apps/desktop-gpui/src/settings/dialog.rs` | C (mode), E (snooze intervals) | Separate sections. Merge C, then E. |
| `apps/desktop-gpui/src/agents/*` | E | E alone (woke rows, Snooze, fork titles). C does not touch it; `/btw` uses the existing branch command. |
| `apps/desktop-gpui/src/chat/*` | C, then D2 | D2 starts after C is merged and owns only `chat/details.rs`. |
| `apps/desktop-gpui/src/git/*`, `editor/` tab reuse | D1 | D1 alone. |
| `apps/mobile/src/*` | F | F alone, apart from generated protocol code. |

## Waves

| Wave | Tasks | Starts when |
| --- | --- | --- |
| 0 | A | The owner approves this plan. |
| 1 | B, C, D1, E, F | A is merged into `gpui/chat`. |
| 2 | D2 | C is merged. |
| 3 | Root integration, then `gdev_qa_chat` | Everything is merged. |

Merge order within wave 1: B, E, C, D1, F. After each merge root runs
`cargo fmt --check`, clippy and `cargo test --workspace`, and fixes integration
breaks itself.

## Final integrated QA (`gdev_qa_chat`)

A fresh agent that reports observations and edits nothing. It drives the merged
build with real `claude` and `codex` on an isolated tmux socket and runtime
directory, headless:

1. Chat mode on; typing the `claude` alias turns the tab into a chat.
2. A file edit in Auto mode gives an edit row; clicking it opens that file's
   hunks in the Git diff tab.
3. Stop ends a long turn.
4. Editing the first message drops everything after it.
5. `/` lists a skill installed for that agent.
6. A helper restart mid-turn reconnects and the turn finishes.
7. `/btw` mid-turn opens a fork in a split; the main turn keeps running.
8. Steps 1 to 4 with `codex`.
9. A closed session resumes from History; a one-minute snooze wakes as a
   placeholder.
10. Terminal mode on; a new `claude` starts as a terminal and the running chat
    is unaffected.

### Phone

No agent can run the phone app on a device here. Task F is covered by type
checks and component tests; the owner does the device pass after the final QA,
using the phone mocks as the checklist.

## Status

| Task | Branch | State | Commit | Notes |
| --- | --- | --- | --- | --- |
| A | `gpui/chat-a` | Merged | `5b1dbbb` | Fast-forward. 4 review rounds, worker QA and a recheck on the final binary passed. `PROTOCOL_MAJOR` 5. Departures from plan §4 are in the plan under "As built by stream A". LOAD_OLDER stays a response with a revision (root decision) |
| B | `gpui/chat-b` | In progress | | Dispatched |
| C | `gpui/chat-c` | In progress | | Dispatched |
| D1 | `gpui/chat-d1` | In progress | | Dispatched |
| D2 | `gpui/chat-d2` | Not started | | |
| E | `gpui/chat-e` | In progress | | Dispatched |
| F | `gpui/chat-f` | In progress | | Dispatched |
| Integration | `gpui/chat` | Not started | | |

Next step: wave 1 finishes; root merges B, E, C, D1, F in that order, then dispatches D2.
