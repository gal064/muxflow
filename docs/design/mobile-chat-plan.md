# Phone chat pass — what changes and how

Branch `gpui/chat`. The owner approved every section of
[mobile-chat-mock.html](mobile-chat-mock.html) as drawn, with the recommended
answer to each decision (18 Nov). The mock is the specification of what the
screen looks like and says; this file says how it is built, in which order, and
what must stay true. Where the two disagree about the screen, the mock wins.
[chat-plan.md](chat-plan.md) §5 F describes the phone chat as first built, and
[chat-polish-plan.md](chat-polish-plan.md) the desktop pass whose features the
phone now catches up with.

## 0. Decisions

| # | Decision | Answer |
| --- | --- | --- |
| D1 | Tool calls while the agent is still working | The count so far and the newest step; tap the line for all |
| D2 | Where a question or permission request sits | Fixed above the reply field, foldable to one line |
| D3 | Stopping a background task from the phone | A Stop button per task, where the agent supports it |
| D4 | Where message actions live | Hold any message for a menu; the newest reply also shows Copy and Fork under it |
| D5 | Fork from the phone | Yes: "Fork from here" opens the fork as a new chat |
| D6 | Photos and files | Show them and send them |
| D7 | Model and effort at a glance | On the header's second line, next to the state |
| D8 | Branch and changes in Details | Branch, changes against the default branch, uncommitted |
| D9 | Loading earlier messages | By itself when you reach the top |

## 1. Rules for whoever implements this

- `CLAUDE.md` holds. The phone updates on its own and meets a host only on an
  equal `PROTOCOL_MAJOR` (5). Nothing here bumps it. For every request the
  phone starts sending, §4 answers what a previous released host does with it.
  No compatibility arms on either side.
- MIT only. No new dependency without it being named in this file.
- The words on screen are the mock's, and where the desktop already says the
  same thing, the desktop's exact strings (`apps/desktop-gpui/src/chat/timeline.rs`,
  `view.rs`): "Working…", "Thinking…", "Waiting on 1 subagent…", "Compacting
  context…", "Stopping…", "12 steps", "7 steps so far", "read 2 · searched 1 ·
  1 subagent", "2 background tasks", "Copy message", "Fork from here".
- Pure rules (grouping, labels, what a row says) live in plain TypeScript
  modules with unit tests; components stay thin. Component tests render from
  scripted events, as `ChatView.test.tsx` does.
- The phone is dark only and uses `src/ui/tokens.ts`. Icons are drawn with
  `react-native-svg`, not text glyphs.
- Every touch target is at least 44 dp (with `hitSlop` where the drawing is
  smaller). Text in fixed-height chrome takes `fixedChromeText`.
- Commands, from the repo root. Never `pnpm mobile:test` bare: its live tests
  build the host with cargo.
  - `pnpm --filter @muxflow/mobile check`
  - `pnpm --filter @muxflow/mobile exec vitest run src/features/chat` while working
  - the whole suite once per package, against a host binary that is already
    built: `MUXFLOW_HOST_BINARY=<built muxflow-host> pnpm --filter @muxflow/mobile test`
  - host: one cargo command at a time, niced, through the lock wrapper:
    `PATH="$HOME/dev/muxflow-gpui-tools/locked:$PATH" nice -n 19 cargo test -p muxflow-host --bin muxflow-host -j 8`
- This machine is a live Muxflow host. Nothing here starts, stops or replaces
  the installed daemon or touches its folders.

## 2. Packages

Built in this order. M2 and M5, and then M3 and M6's screens, were built side
by side in the same files, so each pair shares a commit.

| Pkg | Mock | Problems | What | State |
| --- | --- | --- | --- | --- |
| H1 | — | — | Scripted chat: demo turns for the states it could not show | bbaaf3d0 |
| M1 | S1, S3 | 1–5, 8–10 | The conversation as one anchored list; status line and request card above the reply field | d49ff1b4 |
| M4 | S6 | 18 | Chat's own Markdown renderer | b687b4f2 |
| M2 | S2, S4 | 6, 7, 11, 12 | Steps grouped; subagent rows; background tasks | 138aab07 |
| M3 | S5 | 13–17 | Message menu, copy, fork, edit, time, drafts | 776c5440 |
| M5 | S8, S9 | 20–22 | Header, Details with Git rows, the smaller things | 138aab07 |
| M6 | S7 | 19 | Attachments shown and sent | bf8790af, 776c5440 |

### H1 — scripted chat

`apps/host/src/service/chat/scripted.rs` only. A message that is exactly one of
`demo markdown`, `demo long`, `demo subagent`, `demo background`, `demo compact`
or `demo attachments` runs that demo; any other message rotates question and
permission turns as before. Test-only code, behind `MUXFLOW_CHAT_SCRIPTED=1`.

### M1 — the list, and what sits above the reply field

Problems 1–5 have one cause: the conversation is a plain `ScrollView` of every
item, "at the end" is derived from scroll events that also fire when the
keyboard resizes the view and during the app's own scrolling, and earlier
messages are prepended with nothing holding the reader's place.

- The conversation becomes one inverted, virtualised `FlatList` whose data is
  a row model built by a pure function from the pane (newest first). Offset 0
  is the newest message, so the chat opens there, stays there when the
  keyboard resizes it, and follows new text only while the reader is within
  about 80 dp of the end. Away from the end, new rows and growing text do not
  move what is being read.
- Earlier messages load by themselves when the top comes within reach (D9),
  under a "Loading earlier messages…" line; "Earlier messages are not
  available." stays for an incomplete history. Loading does not move the
  reader.
- A **Latest** pill shows whenever the reader is away from the end; its dot
  shows once anything arrived below since they left it. Tapping it goes to the
  end.
- The working indicator leaves the conversation. A **status line** sits above
  the reply field while a turn works or the context is being compacted:
  spinner, the desktop's label (§3: it names the running tool, or says
  "Thinking…", "Working…", "Waiting on N subagents…", "Compacting context…",
  and "Stopping…" from the moment Stop is pressed), the time since the turn
  started, and "N steps so far" when there are any. The time ticks once a
  second and only while the screen is focused. It does not show while the
  agent waits on a request; the request card says that.
- A pending request leaves the conversation too (D2): the card sits between
  the conversation and the reply field, at most about half the screen tall,
  scrolling inside itself when longer. Its header has a fold arrow; folded it
  is one line, "The agent asked a question" / "The agent is asking for
  permission", with **Answer**. Focusing the reply field folds it; tapping the
  line unfolds it and closes the keyboard. Typing in the card's own "Other"
  field never folds it. A new request arrives unfolded.
- Voice mode's reply player stays the conversation's last row.

### M4 — Markdown

A new `ChatMarkdown` under `src/features/chat/markdown/`, native views only,
parsing with `marked`'s lexer (already in the workspace at 18.0.9; moved to the
phone's dependencies). Code in a box with its language and Copy, scrolling
sideways; tables as tables; nested lists; every run selectable. Voice replies
keep `VoiceMarkdown`.

### M2 — steps, subagents, background tasks

- Grouping is the desktop's (`timeline.rs`, §3), ported as a pure function
  over the row model with the same summary strings. A finished group is one
  line, folded; opened, its rows are plain (no boxes), reasoning as a dim
  italic row. Rows outside a group are plain too.
- While its turn is still working, the newest group shows "N steps so far" and
  under it only the newest step (D1); opening it shows all.
- A row opens only when it has output, or its subject was cut short.
- A subagent row is two lines: the agent type as its verb and the task, then
  what it is doing now with its tool count and tokens; finished, the figures
  and how long it took. A tool that went to the background says "in
  background".
- `ChatState.background` draws a chip above the reply field ("2 background
  tasks") for as long as it is not empty, turn or no turn. The chip opens a
  sheet listing each task with its kind and time running, and **Stop** where
  `stoppable` (D3), sending `CHAT_ACTION_STOP_TASK`.
- Assistant text is drawn with `ChatMarkdown`.

### M3 — messages

- Holding any message opens a sheet (D4): **Copy message**, **Select text**
  (the message's text in a sheet where it can be selected), **Fork from here**
  when the agent can fork (D5), and on the user's own rewindable messages
  **Edit and resend**. The sheet's first row carries the message's day and
  time.
- The newest finished reply has a row under it: Copy, Fork, and its time.
- Fork sends `CHAT_ACTION_FORK` with the item, waits for the fork's pane as
  the side question does, and opens it.
- Editing focuses the field and opens the keyboard, outlines the message being
  edited and dims everything after it; the system Back button cancels the edit
  before it leaves the chat.
- A draft is kept per chat until it is sent or the app is closed.

### M5 — header, Details, smaller things

- Header: a second line with the state (Working, Waiting for you, Idle, Ended)
  and the model and effort (D7); tapping the title opens Details. Voice and
  Details buttons are drawn icons.
- Details: host, folder as the desktop names it, branch, changes against the
  default branch, uncommitted (D8). The controls are unchanged.
- Workspaces: a window whose pane is a chat says so, with its agent's state
  when the host has one.
- Voice mode with voice not set up keeps the text field and shows the setup
  card.
- Send with nothing typed is plain grey.

### M6 — attachments

Shown: a chip per attachment, with a thumbnail for the images this phone
attached; an image chip opens the image full screen. Sent: a plus button
(only when the provider's `attachments` capability is set) opening Photo
library, Take a photo, File; chips on the reply field with upload progress
and a remove button; Send waits for uploads; a refused or failed upload says
why on its chip. Facts in §3, the wire in §4.

### Built differently

- A reply's text is not selectable in place: a hold opens the message sheet,
  and Select text there shows it selectable. Code blocks keep Copy.
- Fork from a user's own message says the fork has the reply too, because the
  host copies through the end of the turn.
- The steps line reads title, counts, failed, line counts, as the mock draws
  it; the desktop puts failed before the counts.
- The newest reply's row is drawn in the reply's own cell, not as a list row.
- A finished subagent shows its tools and tokens and not how long it took:
  the host reports when a subagent started, not when it ended.
- Files on the reply field are not kept when the chat is left; the typed
  text is.
- Attachment chips sit inside the user's bubble, above the text. Every chip
  on the reply field has its cross in a column of its own, so it is 44 dp.
- A failed upload has no Retry: the chip is removed and the file picked again.
- Editing a message brings its files back onto the reply field, as on the
  desktop.
- Git rows show only for a chat in its window's active pane: the host's
  `RESOLVE_ACTIVE_ROOT` answers for no other. A chat in another split shows
  its folder relative to home and no Git rows.
- The workspace row keeps its state in the pill it had; the second line says
  "Chat" and the agent's name.

## 3. Facts the packages rest on

Read from the code at `fff127d0`. Function names are the anchor.

### Steps (M2) — `apps/desktop-gpui/src/chat/timeline.rs`, `view.rs`

- A step is a TOOL or REASONING item (`is_step`). A run is a maximal stretch
  of consecutive steps; any USER, ASSISTANT or NOTICE item ends it (`layout`).
- A run of three or more drawn steps is one group (`GROUP_FROM = 3`); shorter
  runs stay single rows. The group's key is its first item's id. It is live
  only when it is the trailing run and the turn is Working or Waiting.
- Not drawn (`is_drawn`): reasoning with blank text, and reasoning that is
  still streaming while the turn is busy (the status line says "Thinking…").
- Summary (`summary`), joined with " · ": thinking ("thought" / "thought N
  times"; the phone does not time thoughts), "read N", "ran N", "searched N",
  "fetched N", "called N", "1 subagent" / "N subagents", "edited 1 file" /
  "edited N files" by distinct path ("edited N" when none names a path), "N
  other". Title `steps_label`: "N step(s)", plus " so far" when live; then
  "N failed" in the danger ink; the counts are left out while live; then
  `+added −removed` over the edits that did not fail.
- Rows: verb is `tool.title`, else by kind run, read, edit, search, web, mcp,
  agent, tool. Outcome: edit line counts, else `result_summary`, else
  "running", "failed" or "done".
- Status line: `doing(items)` over the trailing run, newest first. Streaming
  reasoning is "Thinking…"; a running tool that is not a subagent is
  "Running {detail}…", "Reading …", "Editing …", "Searching …", "Fetching …",
  "Calling …" (`running`); otherwise by the number of running subagents
  "Working…", "Waiting on 1 subagent…", "Waiting on N subagents…". Above it
  "Compacting context…", above that "Stopping…" (cleared when the turn ends,
  on refusal, or after 10 s). Time: `elapsed`, "42s", "1m 04s", "1h 05m",
  from when the turn was seen to start, or after a snapshot from the newest
  USER item's time.
- Subagent: name is `agent_type` (or "Agent") and the first line of `detail`;
  running line `activity · figures · elapsed` (`subagent_line`), figures "1
  tool" / "N tools" and "{tokens} tokens" with `tokens()` giving 950, 19.7k,
  61k, 182k, 1.3M.
- Background tasks: `background_label`, "1 background task" / "N background
  tasks"; a row per task with a glyph by kind, its description, `elapsed`
  since `started_at_unix_millis` (left out when that is 0), and Stop only when
  `stoppable`. A row being stopped dims until the task leaves the list or 10 s
  pass.

Where the phone departs from the desktop, on purpose:

- The desktop moves running subagents out of the timeline to sit under its
  working line. The phone keeps them in the steps group, two lines each, and
  a live group that is not opened shows every running subagent and the newest
  other step (D1).
- The mock draws "1 step" and "2 steps" lines. The phone follows the desktop
  instead: fewer than three steps stay plain rows, because a line that hides
  one row saves nothing.

### Fork and stopping a task (M2, M3)

- `CHAT_ACTION_FORK` takes `pane_id`, `expected_server_identity` and
  `item_id` (any item of the turn; the copy runs through the end of that
  turn) and answers `forked_pane_id`, a new pane that History lists. The
  desktop sends the id of the reply's last ASSISTANT text. The newest turn
  can be forked only when the turn is Idle. Refusals carry their own words
  (`chat_fork_unavailable`, `chat_item_not_found`, `chat_turn_busy`,
  `chat_not_ready`, `chat_unavailable`), which `refusalText` already shows.
- `CHAT_ACTION_STOP_TASK` takes `task_id` from `ChatState.background` and
  answers nothing; the task leaves the list by a later state event.
  `chat_task_not_found`, `chat_task_not_stoppable`.

### Git rows (M5)

- The folder's root and token come from the phone's existing `resolveRoot`
  (`src/features/files/activeRoot.ts`), which is the Git worktree root when
  there is one.
- `OPERATION_GIT_BRANCH_SUMMARY` on the control lane with `GitRequest{
  operation_id, root, root_token, expected_server_identity, summary_base }`
  answers `GitBranchSummary{ base_ref, total_file_count, added, removed, … }`.
  `summary_base` unset is against the merge base with the default branch
  (`base_ref` "origin/main", shown as "main"; empty when there is none);
  `GIT_SUMMARY_BASE_HEAD` is the uncommitted work, and its answer's
  `base_ref` is "HEAD".
- The branch name is `GitBranchList.current` from `OPERATION_GIT_BRANCHES`
  (empty on a detached HEAD: "Detached HEAD").
- The phone has no Git watch. It reads when Details opens, and again when a
  turn ends while Details is open.
- Folder label (`folder_label` in `details.rs`): inside a repository, the
  repository folder's name and the path within it; otherwise `~`-relative,
  home being the prefix `/home/x`, `/Users/x` or `/root` of the path.
- Not a Git repository (`not_git_repository`): no Git rows.

### Attachments (M6)

- Upload is the terminal upload, on one bulk connection of its own from start
  to end (`openUploadConnection()`; the upload closes it, so it cannot be the
  lane the file viewer shares): `PREPARE_TERMINAL_UPLOAD` (`destination` and
  `source_name` the file's name, `total_bytes`, `collision_policy` RENAME,
  transfer id a UUID in both `operation_id` and `transfer_id`), then
  `WRITE_TERMINAL_UPLOAD_CHUNK` with `offset`, `total_bytes` and at most 1 MiB
  of `content`, each answered with the next offset, then
  `COMMIT_TERMINAL_UPLOAD` with `total_bytes` and the hex BLAKE3 of the
  bytes, answered with `UploadDescriptor.final_path` (accepted only when
  `verified` and the size and digest match). `CANCEL_TERMINAL_UPLOAD` with the
  transfer id, or dropping the connection, removes a partial. Client code to
  mirror: `crates/desktop-core/src/connection/files/upload_manager.rs`.
- The phone has no BLAKE3. `@noble/hashes` (MIT, pure JavaScript) is added
  for it.
- SEND takes at most 10 attachments, each `{ path: final_path, name }`
  directly in the host's upload folder. The host works out type and size
  itself. Text may be empty when there are attachments. An image whose
  base64 is at most 5 MiB, within 6 MiB a message, goes to the agent as an
  image; anything else goes as its path under the text.
- The desktop limits: 10 attachments, 25 MiB an image.
- A thumbnail needs the bytes. For what this phone attached, the picker's
  local file is the preview, as on the desktop. For anything else the bytes
  can be read with no host change (`RESOLVE_TERMINAL_FILE` gives a
  single-file capability for an absolute path, `OPEN_FILE_STREAM` streams an
  image up to 25 MiB), but only the whole file, so the phone does not fetch
  them unasked: those attachments are a named chip, and tapping an image chip
  loads and shows it full screen. An image the agent's record holds inline
  has no path and no bytes on the wire: a chip that does nothing. This is
  what the desktop does, and departs from the mock, which drew a thumbnail
  for every image.
- New native parts: `expo-image-picker` and `expo-document-picker`, at the
  versions Expo 57 pins.

## 4. Wire

One additive field: `ChatCapabilities.attachments = 9`, true for a provider
that takes attachments on SEND (all three do). The phone offers the plus
button only when it is set.

| The phone newly sends or reads | New phone, previous released host | Previous phone, new host |
| --- | --- | --- |
| `ChatCapabilities.attachments` | Reads false: no plus button, so nothing is uploaded that the host would drop | Ignores the field |
| SEND with `attachments` | Not sent: the capability is false | — |
| Terminal upload on the bulk lane | Not started: the capability is false | — |
| `CHAT_ACTION_FORK` | Refused with "Unknown chat action.", shown as a toast | — |
| `CHAT_ACTION_STOP_TASK` | Never sent: that host lists no background task | — |
| `ChatState.background`, `compacting`; `ChatTool` fields 11–15; `ChatItem.attachments` | Reads empty values: no chip, no figures, no attachment | Already ignored |
| `GIT_BRANCH_SUMMARY` with `summary_base`, `GIT_BRANCHES`, `RESOLVE_ACTIVE_ROOT` | Not met: see below | — |
| `RESOLVE_TERMINAL_FILE`, `OPEN_FILE_STREAM` for an image | Already used by the phone | — |

The previous released host is v0.1.32, which is protocol major 4 and is
refused at admission, so the middle column describes a host the phone never
talks to. The v0.1.33 release candidates are major 5 and are not releases:
against one of them Fork is offered and refused ("Unknown chat action."), and
Uncommitted shows the default-branch counts, because that host ignores
`summary_base`. The phone carries no check for either, as CLAUDE.md asks.

No major bump: nothing breaks or misleads against a released build in either
direction.

## 5. Verification

Per package: `check`, the chat tests, the whole suite once. At the end: one
review round by fresh reviewers (code, invariants, design), then manual QA in
the Android emulator against a separate dev host with the scripted chat, every
section of the mock walked, and one short real Claude Code chat.

Not verifiable here: the iPhone build.
