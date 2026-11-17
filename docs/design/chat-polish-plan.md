# Chat polish pass — root causes and fix plan

Branch `gpui/chat`. This file is the engineering plan for the thirteen
problems the owner filed against the first chat build. The visible changes
are drawn in [chat-polish-mock.html](chat-polish-mock.html); **no visible
change is implemented before the owner has approved its section of the
mock**. Where the two disagree, the mock's "Decisions for the owner" table
wins and this file is wrong. [chat-plan.md](chat-plan.md) is still the
description of what was built; this plan only says what changes.

Line numbers are as of `07cd7e5d` and drift; function names are the anchor.

## 0. Summary

| # | Problem | Root cause | Fix | Layers | Wire | Size | State |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Paste image / drop file does nothing | Chat has no paste or drop handler; the upload glue is typed on `TerminalPane`; a chat message is text only on the wire and in both providers | Attachments: generalise the upload batch, chips in the composer, `attachments` on SEND and on USER items, providers hand images over as images | desktop, wire, host | additive | L | built, `470c97b3` |
| 2 | Folder path is absolute | Details card prints `Pane.current_path` as it is | `~`-relative, or repo name + path inside the active repository | desktop | none | S | built, `70740c88` |
| 3 | Text symbols as icons | Card rows, pills and Send draw Unicode glyphs; only the Kit's 101 default icons are embedded | Embed the needed Lucide icons with `icon_assets!` and use them | desktop | none | S | built, `70740c88` |
| 4 | "Changes vs main" ambiguous, no uncommitted-only | One summary exists: working tree against the merge base with the default branch | Second summary against `HEAD`; two rows | desktop, wire, host | additive | M | built, `70740c88` |
| 5 | Pill says "Default (recommended)" | Host passes Claude Code's `displayName` through | Host labels the default entry with the model it resolves to | host | none | S | built, `70740c88` |
| 6 | Every tool call its own row | Timeline rows are 1:1 with items | Client-side grouping of tool/thinking runs | desktop | none | M | built, `11664539` |
| 7 | Nothing shows while the agent works | By design: the first mock drew no working mark; thinking is shown only when the model streams thinking text; a subagent's activity is dropped by the host | Status line while a turn works; thinking as a step; subagent step count | desktop, host | none | M | built, `11664539` |
| 8 | Weak message buttons, no fork | Edit is a text button left of the bubble; copy only in the context menu; no wire action forks at a message | Icon rows under messages; `CHAT_ACTION_FORK` | desktop, wire, host | additive | S + L | built, `11664539` (buttons), `8fe44838` (fork) |
| 9 | Esc does not stop the agent | The composer's Escape handler ends after "close list / cancel edit" | Third arm: interrupt while WORKING; also outside the composer | desktop | none | S | built, `7675bee6` |
| 10 | Colours not compared with T3 Code | Not done | Compared (§3.10): four kinds of small text are under 4.5:1; three options | desktop | none | S | built, `5ff96360` (C1) |
| 11 | Typing does not clear the notification | Chat calls `note_activity` only on send and answer | Call it on any key or press in the pane | desktop | none | S | built, `7675bee6` |
| 12 | Slow on a 1 s link | (a) the chat pane is destroyed, closed at the host and reloaded on every tab switch; (b) options and Stop draw only host-confirmed state; (c) local branch lacked origin's two latency fixes | (a) keep panes alive; (b) optimistic overlay; (c) synced | desktop | none | M | built, `7c9532d2`; the two optional items not built |
| 14 | No sign of background tasks, of what a subagent does, or of compacting (added 16 Nov) | The host reads none of it: Claude Code's `task_*`, `background_tasks_changed` and `status: compacting` lines fall through `system`'s `_ => {}`; Codex's compaction item is drawn as "compacted" the moment it starts | Host keeps a task list and a compacting flag in `ChatState`, and live figures on a subagent's row; the desktop draws a chip by the composer, subagent rows under the status line, and "Compacting context" | desktop, wire, host | additive | M | built, `f595ad2b` |
| 13 | Cmd+C in the chat copies nothing (Mac) | The app's shortcut interceptor runs `terminal.copy` before the focused text sees the chord | Selectable chat text keeps its own chords | desktop | none | S | built, `e27bd5aa` |

Sizes: S under a day for one agent, M one to two days, L more.

## 1. Rules for whoever implements this

- `CLAUDE.md` holds: desktop and host change together with no compatibility
  arms; the phone is the only independent client; MIT only; do not read Zed's
  GPL crates; `gpui-kit` stays at `=0.9.0`; kill dev daemons when done.
- **Mock first.** A work package that changes anything visible starts only
  after the owner approved the mock section named in its row above, with the
  decisions (D1…D18) answered. Build what the mock shows, not what this file
  describes, when they differ.
- **Wire first, once.** Every protocol addition in §4 lands in one commit
  before the packages that use it, with the phone's generated code
  (`pnpm --filter mobile protocol:gen`; `protocol:check` gates it). None of
  them needs a `PROTOCOL_MAJOR` bump (§4 answers both directions).
- **Motion.** `ui/motion.rs` records a measured rule: an indicator that
  animates costs CPU for as long as it is on (about 5 % of a core for six
  working marks). The status line of §3.7 holds still and ticks once a second
  unless the owner chooses the animated alternative (D11), and then only on
  the shared clock (`motion::frame`), never `with_animation`.
- Tests in the style already there: `chat/tests.rs` for views, the provider
  fixtures under `apps/host/src/service/chat/fixtures/` for the host, the
  scripted provider for anything end to end.
- **One cargo command at a time, and a narrow one.** `cargo test -p
  muxflow-gpui` with no target links eleven integration binaries of over a
  gigabyte each; two of those at once stalled this machine's disk and took
  the Muxflow host on it down (docs/design/host-disk-stall-plan.md). Use
  `--lib` or `--test <name>`, never two builds side by side, and never from
  two agents at once.
- Provider facts marked **to verify** in §5 are checked against the real
  agent first; the result is written into this file where the claim is.

## 2. Done in this pass

### Branch sync (12c)

Local `gpui/chat` was 332 ahead and 346 behind `origin/gpui/chat` because
origin's history had been rewritten, not because local held different work:
`git cherry` found 226 commits equal by patch on both sides; local's five
others were three rebased duplicates (palette, agent-row selection, the
migration plan) and a `CLAUDE.md` change with its own revert. The tree diff
touched nothing under `chat/`. Local was moved to origin (`07cd7e5d`), which
brings `a3a8a428` "Send host requests without waiting for the UI thread" and
`3d3e7983` "Run queued work before an X11 frame". The old tip is kept as
`backup/gpui-chat-before-origin-sync-20261116` (`efaa57ca`); delete it once
nothing is missed.

### 13 — Copy from the chat

Root cause, confirmed by a test that fails without the fix.
`commands::on_keystroke` (`commands/mod.rs`) decides what the app does with
every keystroke before the focused element acts on it. `Meta+C` / `Ctrl+C`
is the command `terminal.copy` (`commands/registry.rs`), which is available
whenever a pane is active, a chat's included. Only a focused Kit *input* or
the Live Markdown key context was exempt. The agent's prose is a Kit
`TextView` (`chat/view.rs`, `assistant_row`): it keeps focus after a
selection and binds `cmd-c` / `ctrl-c` in its own `"TextView"` context, but
was neither. So the chord ran `terminal.copy`, which acts on the terminal
last focused, and `stop_propagation` kept it from the text. On Linux
`Ctrl+C` with no terminal selection is passed on, so it showed there only
while some terminal still held a selection: hence "at least on Mac".

Fix. `on_keystroke` counts the `"TextView"` context as a text field, as it
does Live Markdown. Selected prose keeps the chords it binds (copy, select
all); every other app shortcut still runs. The Markdown preview tab had the
same bug and is fixed by the same line. One side effect, also a fix:
Cmd/Ctrl+V with prose focused no longer pastes into the last terminal.

Tests. `tests/commands.rs`
`selected_prose_keeps_copy_and_select_all_but_not_the_apps_shortcuts`
(drag-select, native copy chord, clipboard checked; fails with the fix
taken out) and a disposition test in `commands/keymap.rs`. **Owner's check
on a Mac**: built on Linux only; the Mac path runs in the macOS CI job and
wants one manual Cmd+C.

### 9 — Esc stops the agent

Root cause. `composer_area`'s Escape handler (`chat/composer.rs`) had two
arms, close the command list and cancel an edit, and Stop existed only as
the Send button. Nothing listened for Escape outside the field.

Fix. A chat-scoped binding (`chat::Escape`, key context `"Chat"` on the
pane's root) calls `ChatPane::escape`: a composition, then the command
list, then the edit, then `interrupt` while the turn is WORKING and the
link is live (`stoppable`, which the Stop button now shares); otherwise the
key goes on. The binding is the shallowest in the pane, so an open menu,
the branch popover or a field takes Escape first. WAITING is not stopped:
the card is answered. One press (D10).

Tests in `chat/tests.rs`: stops while working from the composer and from a
button, an open menu only closes, the list and the edit come first, nothing
is sent while idle or waiting. Not tested: a real IME, and the branch
popover (read from the Kit's source).

### 11 — Typing clears the notification

Root cause. A terminal reports `note_activity` on every key
(`terminal/input/mod.rs`) and every press (`terminal/pane.rs`). The chat
called it in `submit` and `answer` only (`chat/pane.rs`).

Fix. The pane reports it for any key while the keyboard is inside it
(`observe_keystrokes`), for every composer change, and for any press on the
pane. The hook drops repeats, as it does for a terminal. Test:
`typing_or_pressing_in_a_chat_says_its_agent_has_been_seen`.

Committed as `e27bd5aa` (13) and `7675bee6` (9, 11). `cargo test -p
muxflow-gpui --lib -- chat:: commands::` (127 passed) and `--test commands`
(50 passed) on the tree with all three; the other integration tests were
not run.

## 3. The rest, one by one

### 3.1 Attachments: paste an image, drop a file (L)

**Root cause.** Three separate gaps, each of which alone would make it "do
nothing":

1. *No handler.* A terminal pane gets its paste and drop listeners from
   `terminal::integration::attach` (`integration/mod.rs`): the `Paste` action
   reads the native clipboard (files, a staged PNG, or text) and
   `.on_drop::<ExternalPaths>` / `<InternalPathDrag>` take drops. A chat pane
   is never passed through `attach`, and the composer is a Kit `Textarea`
   whose own paste takes clipboard *text* only.
2. *The upload glue is a terminal's.* `integration/upload.rs` owns the batch
   (preflight, collision review, upload, verified destination), but `Batch`
   holds a `WeakEntity<TerminalPane>`, reads its scope from a `TerminalPane`,
   keeps its busy flag on `pane.integration`, and ends by pasting shell-quoted
   paths into the terminal. Nothing below that is terminal-specific:
   `desktop-core`'s upload manager and the host's `terminal_upload.rs` move a
   file into the host's staging directory and answer with its path.
3. *A message is text.* `ChatRequest.text` is all SEND carries. Claude's
   provider writes `"content": text` (`claude.rs`, `SessionCommand::Send`);
   Codex's builds `[{type: "text"}]` plus skill items (`codex.rs`, `input`).

**Design.**

- *Desktop, taking files.* Split `upload::Batch` from its target: a small
  `UploadTarget` (scope now, set busy, deliver the verified host paths) with
  two implementations. The terminal's delivers by pasting, exactly as today.
  The chat's adds attachments to the composer. As built, a local host
  stages the file too: it is copied into the host's upload folder, because
  the host admits only files that lie there (below). The agent is given the
  copy's path, not the original's.
- *Desktop, the three ways in.* On the chat's root: `on_drop` for
  `ExternalPaths` and `InternalPathDrag`, with a pane-wide drop hint like the
  terminal's. On the composer's wrapper: `capture_action` of the Kit's
  `input::Paste`, which reads the native clipboard the way
  `integration::paste` does; files and an image become attachments, text is
  inserted at the cursor as the field would have.
- *Desktop, state.* `ChatPane.attachments: Vec<Attachment>` (id, name, host
  path once known, local preview path, upload transfer id, state). Progress
  comes from the existing `Uploads` records by transfer id. Send is refused
  while one is still uploading; a failed one offers retry and remove. The
  draft kept across unmounts keeps them. Editing an earlier message puts its
  attachments back with its text.
- *Wire* (§4): `ChatRequest.attachments` on SEND, `ChatItem.attachments` on
  USER items.
- *Host.* `attach::admit` takes only regular files of the host's user that
  lie directly in the upload folder (`~/.cache/muxflow/uploads`, opened
  with `O_NOFOLLOW`), at most ten. An image is told by its first bytes, not
  its name. Then per provider:
  - Claude: an image (png, jpeg, gif, webp) small enough for the API (5 MiB
    encoded) goes as an `image` content block, base64, beside the text
    block, up to 6 MiB encoded in one message, so that a message always
    fits the host's 16 MiB line bound and the history reader's first
    window.
    **Verified** on Claude Code 2.1.326: a `user` line whose `content` is
    `[text, image]` is answered about the image. Anything else, and an image
    too large, goes as its path on a line of its own under the text; the
    agent reads it with its own tools.
  - Codex: an image goes as `{type: "localImage", path}`, any other file as
    its path under the text, as for Claude. **Verified** live on 0.169.0
    (§5); a `mention` item is accepted but the server does not read the
    file in, so it is not used.
  - History: Claude's session file holds the image inline, so a reloaded
    message shows an unnamed "Image" chip with no path; staged paths under
    the text come back as chips. The reader's window grows (8 MiB, doubling
    to 64 MiB) only when it holds little of the conversation, as after a
    few messages with images, or not the reply a fork is cut at.
- *Cleanup.* The staging directory already forgets completed uploads after
  seven days. A chip whose file is gone opens nothing and says so.

**Tests.** The batch against both targets (the terminal's existing tests keep
passing unchanged). View tests: drop, paste of each clipboard kind, send with
a pending upload refused, chips in the sent bubble. Host: each provider's
line for text + image + file, from fixtures; `admit` refusals.

**Risks.** The batch is delicate (scope changes mid-upload, collision
review). Keep its logic untouched and move only its ends. Whether Claude Code
downsizes an oversized inline image is **not known**; the 5 MB rule avoids
needing to know.

### 3.2 Folder path (S)

**Root cause.** `details.rs` `card` draws `self.shown.path`, which is
`Pane.current_path`, with `text_ellipsis_start`.

**Design.** One pure function, `folder_label(path, home, repo_root)`:
inside the repository the card already follows (`Card::repository` gives
`worktree_root`), the root's own name and the rest (`muxflow/apps/host`);
otherwise `workspaces::rows::abbreviate_home` with `infer_home` over the
host's pane paths (the helpers the workspace rows use; the app cannot ask the
host for its home without a wire change). The `aria_label` and a tooltip keep
the absolute path. A chat in a repository the Git panel does not follow gets
the `~` form: the card knows no root for it.

### 3.3 Icons (S)

**Root cause.** `details.rs` (`glyph("/")`, `row("\u{2442}", …)`,
`"\u{25cb}"`, `"\u{b1}"`, `"\u{25a3}"`, `"\u{25be}"`), `composer.rs`
(`pill`'s `"\u{25be}"`, `send_button`'s `"\u{2191}"` / `"\u{25a0}"`) and the
tool rows' mono verbs are text. The app registers
`gpui_kit::assets::Assets` (`app.rs`), which embeds only the Kit's 101
default icons; `git-branch`, `git-commit-horizontal`, `pencil`, `git-fork`,
`file-diff`, `paperclip`, `brain`, `server` and the rest of Lucide ship in the
pinned `gpui-kit-assets` crate but are not embedded.

**Design.** `gpui_kit::assets::icon_assets!(ChatIcons, [GitBranch, …])` picks
the extra SVGs at compile time with no copied files, no new dependency and
no Kit bump; `app.rs` registers a source that tries `ChatIcons` and falls
back to `Assets` (the crate's README shows the composition, about 2 KiB an
icon). One helper in `chat/` draws an icon at the row's size and ink. The
set is whatever the owner picks in D1–D2 plus the fixed ones in the mock.

### 3.4 Uncommitted changes beside the branch's (M)

**Root cause.** The card's one row is
`OPERATION_GIT_BRANCH_SUMMARY`: `read_branch_summary`
(`apps/host/src/service/git/branch.rs`) diffs the working tree against the
merge base of `HEAD` and the default branch and adds untracked files. That
is the whole branch plus whatever is uncommitted, under the label "Changes
vs main". The status the Git panel's Uncommitted scope lists carries files
but no line counts, so the card cannot show the other number today.

**Design.**

- *Wire* (§4): `GitRequest.summary_base`, default-branch merge base (zero,
  today's meaning) or `HEAD`.
- *Host.* `read_branch_summary` takes the base. For `HEAD` it diffs against
  `HEAD` (the empty tree in a repository with no commit yet) and adds
  untracked files the same way; `base_ref` answers `HEAD`.
- *Desktop.* `GitPanel` keeps two summaries; `read_branch_summary(reading)`
  becomes a reader count per base. The card draws two rows (wording per D3),
  each opening the panel in its scope (`git::show(Scope::Uncommitted | Branch)`).
  The panel's own scope switch keeps its two scopes.

**Tests.** Host: a repository with a commit ahead of `origin/main`, a staged
change, an unstaged one and an untracked file gives the two expected sums;
an unborn `HEAD`. Desktop: the card's rows from scripted answers.

### 3.5 The default model's name (S)

**Root cause.** `Claude::initialized` (`claude.rs`) sets
`ChatModel.label = displayName`. Claude Code's first entry is
`{value: "default", displayName: "Default (recommended)", resolvedModel:
"claude-opus-5-5", description: "Opus 5.5 · Best for everyday, complex
tasks"}` (read from the installed 2.1.326; fourteen entries in all, the
second being `{value: "opus", displayName: "Opus 5.5", resolvedModel:
"claude-opus-5-5"}`).

**Design.** Host only. For the entry whose `value` is `default`: the
`displayName` of another entry with the same `resolvedModel`; else the part
of `description` before `" · "`; else `resolvedModel`; formatted per D4
(recommended `Opus 5.5 (default)`). The phone gets the same label with no
change. `model_id` already maps a session started without `--model` to
`default`, and one started with `--model opus` to `opus`.

### 3.6 Tool calls grouped (M)

**Root cause.** `ChatPane::row` maps list row `n` to `items[n-1]`, and
`changed` splices the list by item index. One item, one row.

**Design.** Client only.

- A pure function `layout(items, turn) -> Vec<Entry>` in a new
  `chat/timeline.rs`: a maximal run of TOOL and REASONING items is one
  `Entry::Group { first, len }` when it has three or more (D7), otherwise
  plain `Entry::Item`s. USER, ASSISTANT and NOTICE items end a run.
- `ChatPane` keeps the entries beside the items. After `Appended`,
  `Truncated`, `Reset` and a merged page it recomputes them and splices the
  list from the first entry that differs; `Updated(index)` only remeasures
  the entry that holds `index` (an item never changes kind). A group is
  known by its first item's id, which is also the key of its open state.
- A collapsed group is one line: step count, counts by kind (wording per
  D13), the sum of `added` / `removed`, up to three edited files as chips,
  "N failed" when any failed. Open, it is its rows as they are today. The
  last group of a working turn stays one closed line, "N steps so far":
  what is running is said once, by the status line of §3.7 under it.
- Tool rows gain a leading icon by `ChatToolKind`.

T3 Code does the same thing (MIT; read at `4b758f49`, behaviour only): work
between two messages is one line, a lone tool call is a plain row, a group
with something running is one live line, and reasoning is grouped with
tools. It groups from two entries and writes the line as a sentence ("Ran 3
commands and read 2 files"); D7 and D13 are where the owner picks.

**Tests.** `layout` as a table of item sequences. View tests: a run that
grows from two to three items becomes a group without the list jumping; an
open group stays open across deltas; a rewind into a group; a page merged
above a group at the boundary.

**Risk.** The list's scroll anchoring when rows before the viewport merge
into one. Recompute from the first changed entry and let
`MessageScrollerState::splice` do what it does for an edit today.

### 3.7 Working, and thinking (M)

**Root cause.** Three things.

1. *By design, no mark.* chat-plan.md, "As built by stream C": "`WorkingIcon`
   is not used: the mock draws no working mark, and Stop and the composer's
   placeholder say that the agent works." `ChatState.turn` is WORKING the
   whole time and the timeline draws nothing for it.
2. *Thinking only when streamed.* `Claude::stream_event` returns on a
   `thinking` block's start ("Thinking is shown once there is some to show")
   and makes a REASONING item on the first `thinking_delta`. A model that
   keeps its thinking to itself sends none, so a long think is a blank. What
   does arrive is drawn as a dim paragraph (`view.rs`, `item_row`).
3. *Subagents are silent.* `Claude::message` drops every line with a
   `parent_tool_use_id` ("What a subagent says is its own conversation").
   A subagent that works for minutes is one unchanging `agent` row.

**Design.**

- *Status line*, client only: one more trailing row (`Row::Working`, beside
  `Sending` and `Request`) while `turn` is WORKING. Its label comes from the
  newest item: a streaming REASONING item, "Thinking…" and that item's last
  line; a RUNNING tool, its verb and detail; else "Working…". Elapsed time
  counts from when this pane saw the turn start, else from the newest USER
  item's `at_unix_millis`, never below zero; a one-second timer ticks it
  while the pane is drawn. The mark is the still `motion::WorkingIcon`
  (D11). "Esc to stop" at the end.
- *Thinking as a step*: a finished REASONING item is a "Thought" row that
  opens to its text, and joins a group as any step does. No duration on the
  wire: the row says "Thought", and "Thought for Ns" only for one this pane
  watched stream.
- *Thinking without thinking text.* Claude Code sends
  `system/thinking_tokens { estimated_tokens }` while a model thinks, also
  when it streams no thinking text (seen live, 2.1.326). The host turns the
  first one of a block into a streaming REASONING item with no text, so the
  status line says "Thinking…" instead of "Working…"; the item is dropped if
  the block ends with no text.
- *Subagent steps*: replaced by §3.14, which uses the figures Claude Code
  reports itself instead of counting nested lines.

**Tests.** The label for each newest-item case; the row comes and goes with
the turn; a recorded subagent stream gives rising step counts.

### 3.8 Message buttons, and fork from a message (S + L)

**Root cause.** `user_row` (`view.rs`) draws "Edit" as a bordered text
button before the bubble in a `justify_end` row, so it sits to the bubble's
left. Copy is `copyable`'s context-menu entry only. `assistant_row` has no
controls. The wire can fork only the whole conversation with a question
(`CHAT_ACTION_SIDE_FORK`).

**Design, buttons (S).** An icon row under the user's bubble, right-aligned:
copy, edit. One under the last ASSISTANT item of a reply, left-aligned: copy
(the item's Markdown), fork. Shown on hover and focus, each a tab stop with a
name. `copyable`'s menu stays.

**Design, fork (L).**

- *Wire* (§4): `CHAT_ACTION_FORK` with `item_id`; the answer is
  `forked_pane_id`, followed as `follow_fork` already does.
- *Meaning.* The fork holds the conversation through the end of the turn
  that contains `item_id` (D6). Refused while that turn is still running.
- *Host, shared.* Unlike `/btw`, which starts the agent's terminal branch
  command and lets Chat mode convert it, the host starts the fork as a chat
  itself: `split-window` with the provider's `pane_command`, tagged as a
  chat in the same tmux step (what `convert::restart_agent` does for a
  rewind, in a new pane). Title `<name> (fork)`. It is **not** marked as a
  side question: a deliberate fork is a conversation of its own and is
  listed in History (D12).
- *Claude.* `Launch { resume: Session { id, fork: true }, resume_at }`;
  `pane_command` adds `--resume-session-at` for a fork as it does for a
  resume. The cut is the session entry before the next USER message
  (`history::parent_of`, which the rewind uses), or none for the last turn.
  **Verified** on 2.1.326: `claude -p --resume A --fork-session --session-id
  B --resume-session-at <uuid>` gave a fork that knew only what came before
  the cut, and left `A` whole.
- *Codex.* `thread/fork { threadId, lastTurnId }` ("fork through, inclusive;
  the turn cannot be in progress", from the 0.169.0 schema). The provider
  already keeps user item → turn id (`self.turns`); turns read from history
  need their ids kept too. **To verify** against the server.

**Tests.** Host: each provider's launch for a fork at the first, a middle and
the last turn; refusal mid-turn. Desktop: the button sends FORK with the
reply's item and follows the pane.

### 3.14 Background tasks, subagents, compacting (M)

Added on 16 Nov. One package, because the three share one host change: read
the lines the agent already sends about work that is not the turn itself.

**Root cause.** The host drops all of it.

- Claude Code (2.1.326, one recorded headless turn, §5) sends `system` lines
  the adapter does not know, and `Claude::system` ends in `_ => {}`:
  - `background_tasks_changed { tasks: [{ task_id, task_type, description }] }`
    — the whole list of what still runs in the background, on every change.
    It arrived when a `Bash` with `run_in_background` started and again,
    empty, when it ended.
  - `task_started { task_id, tool_use_id, description, task_type,
    subagent_type, is_backgrounded }`, for a background command
    (`local_bash`) and for a subagent (`local_agent`) alike.
  - `task_progress { task_id, tool_use_id, description, last_tool_name,
    usage { tool_uses, total_tokens, duration_ms } }`, for a subagent, as it
    works.
  - `task_updated { task_id, patch { status, end_time } }` and
    `task_notification { task_id, tool_use_id, status, summary, usage }`
    when one ends.
  - `status { status: "compacting" }` while it compacts, then
    `status { status: null, compact_result: "success" }`, then
    `compact_boundary { compact_metadata { trigger, pre_tokens, post_tokens,
    duration_ms } }` (seen live on a manual `/compact`). `Claude::system`'s
    `"status"` arm reads only `permissionMode`. After the boundary comes a
    `user` line holding the summary ("This session is being continued…");
    it is not something the user said.
- A background task outlives the turn: the turn ends, `turn` is IDLE, and
  the command still runs. When it ends, the agent starts a turn by itself.
  Nothing on the wire can say "idle, with one thing still running".
- Codex: `item/started` and `item/completed` both go through
  `codex.rs`'s item reader, and `contextCompaction` becomes the notice
  "Conversation compacted." on *started*, so it says done while it runs.
  `collabAgentToolCall` carries `tool` (`spawnAgent`, `wait`, …) and
  `agentsStates { thread → status }` and is drawn as one fixed row.
  Background terminals are `commandExecution` items whose `source` is
  `unifiedExecStartup`; whether they outlive the turn the way Claude Code's
  do is **to verify** live.

**Design.**

*Wire* (additive, §4):

```proto
enum ChatTaskKind { UNSPECIFIED = 0; COMMAND = 1; SUBAGENT = 2; OTHER = 3; }
message ChatBackgroundTask {
  string id = 1;
  ChatTaskKind kind = 2;
  string description = 3;
  int64 started_at_unix_millis = 4;
  bool stoppable = 5;          // STOP_TASK accepts this task
}
enum ChatAction { … CHAT_ACTION_STOP_TASK = 12; }   // takes `task_id`
message ChatState {
  …
  repeated ChatBackgroundTask background = 11;  // what runs outside the turn
  bool compacting = 12;
}
message ChatTool {
  …
  // SUBAGENT only.
  string agent_type = 11;      // "Explore"
  string activity = 12;        // what it does now: "Running ls -la"
  uint32 tool_uses = 13;
  uint64 tokens = 14;
  bool background = 15;
}
```

*Host, Claude.*

- `background_tasks_changed` replaces `state.background` (kind from
  `task_type`; `started_at` kept per `task_id` from its `task_started`).
  Cleared when the agent process ends.
- `task_started` with a `tool_use_id`: remember `task_id → item id`, set the
  SUBAGENT row's `agent_type` and `background`.
- `task_progress`: set `activity`, `tool_uses`, `tokens` on that row and
  republish it, joined with the existing 50 ms window.
- `task_notification`: final figures on the row, then the task is
  forgotten. The row's own `tool_result` closes it and gives its outcome,
  as today.
- `status`: `compacting = (status == "compacting")`, only for a line that
  has the key and is the conversation's own (not a subagent's); also
  cleared by `compact_boundary`, by `result` and on a reconnect.
- The summary `user` line after a boundary makes no USER item; the fixture
  of the recorded compaction covers it.
- `CHAT_ACTION_STOP_TASK`: the control request
  `{ subtype: "stop_task", task_id }`. Seen live: the task is killed, a
  `task_notification` with `status: "stopped"` follows and the list
  empties. Every Claude task is `stoppable`.
- The notice becomes "Context compacted" with the size before it when
  `compact_metadata.pre_tokens` is there ("Context compacted, was 182k
  tokens").
- A turn the agent starts by itself (a background task ended): confirm that
  `busy` goes true from the first `stream_event` with no SEND before it; a
  fixture of the recorded stream covers it.

*Host, Codex.* `contextCompaction`: `compacting = true` on `item/started`,
false and the notice on `item/completed`. `collabAgentToolCall`:
`spawnAgent` is the SUBAGENT row, its status from `agentsStates`; `wait`,
`sendInput` and the rest update that row's `activity` and make no row of
their own. No token or step figures exist. `background` is filled only if
the live check shows that Codex has such tasks.

*Desktop.*

- **Compacting**: the status line of §3.7 reads "Compacting context… 12s"
  whenever `state.compacting`, whatever `turn` says, with the same still
  mark. The notice row closes it. "Esc to stop" shows only while `turn` is
  WORKING; whether an interrupt ends a compaction is **to verify** live.
- **Running subagents** are rows of their own, under the status line and
  outside any group, one per subagent, because several run side by side for
  minutes. While any runs and nothing else does, the status line reads
  "Waiting on N subagents…" (this replaces §3.7's "Waiting on a subagent"
  wording). `activity` is `task_progress.description` as the agent sends it
  (seen live for a command: "Running List all entries…"; other tools **to
  verify**), falling back to `last_tool_name`. Each row has: mark, `agent_type`, the description, and under it `activity ·
  N tools · Nk tokens · elapsed`. A finished one is an ordinary step and
  folds into its group (§3.6), where its line reads "Explore: Count files ·
  3 tools · 19.7k tokens".
- **Background tasks**: a chip on the composer's top edge, "2 background
  tasks", shown while `state.background` is not empty, working or idle. It
  opens a list: an icon for the kind, the description, elapsed time, and
  a stop button on a `stoppable` row (D18), which sends STOP_TASK and holds
  the row dimmed until the list changes. The
  chip is the only thing that tells an idle chat with a running command
  apart from a finished one, so it does not hide in the details card. A task
  that ends leaves the list, and the chip goes with the last one; nothing
  marks the end there, because the agent then starts a turn and says what
  came of it. On Codex the chip appears only if the live check finds
  background work that outlives a turn.
- Elapsed times tick on the one-second timer of §3.7; no new timer.

**Not in this cut.** A subagent's own conversation (its messages and tool
calls) stays out of the timeline: `parent_tool_use_id` lines are still
dropped. T3 Code makes each subagent a thread of its own; that is a larger
design than the three signs asked for. A Codex task is not `stoppable` until a live check finds a
request that stops one; its row then has no stop button.

**Tests.** The recorded stream as a fixture: the list fills and empties; the
subagent row's figures rise and end; compacting flips on and off; a
self-started turn is WORKING. Desktop: the chip comes and goes with the list
and survives IDLE; a running subagent is not folded; a finished one is; the
status line prefers "Compacting context".

### 3.9, 3.11, 3.13

Done: §2.

### 3.10 Colours (S)

Not a defect with a cause: the comparison was never made. It is now in the
mock's S8, for every role the chat uses, with contrast ratios. T3 Code's
values are its default `:root` tokens (`apps/web/src/index.css`) converted
from OKLCH; translucent ones are flattened over its pane.

What it found:

- *Surfaces.* T3's dark ground is neutral black, Muxflow's the blue-grey
  `#282c34`; the light grounds are nearly the same. T3 sets the user's
  bubble apart by fill alone (luminance step 1.26 from the pane in dark,
  1.16 in light, no border); Muxflow by a small step (1.07, 1.05) and a
  hairline. T3's tool rows have no box. T3's composer is a raised surface;
  Muxflow's is the pane's colour with an outline.
- *Text that is hard to read in Muxflow's chat* (WCAG ratio on the pane,
  under 4.5): `chrome_faint` in both themes (3.86 dark, 3.50 light), used
  for tool verbs, notices and loading notes; `ok` as text in light (3.16,
  "done" and `+N`); `state_working` as text in light (3.05, "running");
  `danger` as text in dark (3.75, "failed" and `−N`); the light link, by a
  hair (4.49999). T3's own faint text is under 4.5 too.

Options, independent of each other, drawn in the mock (D14):

- **C1, recommended: readable small text, chat only.** Text that has to be
  read uses `chrome_dim` instead of `chrome_faint`; red text uses the
  existing `danger_ink`; light green, amber and link text take values that
  pass (`#15803d`, `#b45309`, `#2563eb`, each about 4.8 to 4.9). No surface
  changes. The palette is generated from `packages/client-core/src/palette.ts`
  and shared with the rest of the app and the phone, so new values go in as
  new roles there (an `ok_ink` beside `danger_ink`), not as changes to
  `ok` or `accent`, unless the owner wants them app-wide.
- **C2: bubble by fill.** No hairline, a stronger fill (`#353b45` dark,
  `#e4e4e7` light).
- **C3: raised composer.** One step off the pane, hairline outline.

Not proposed: T3's black ground. Nothing is changed before the owner picks.

### 3.12 Slow over a 1 s link (M)

**Root cause (a): every visit is a cold open.** `TerminalSurface::apply_panes`
unmounts every pane tmux no longer lists for the window on screen. For a chat
that drops the only handle to its `ChatPane`; `ChatPane::released` sends
`CLOSE`, and the model, the measured list, the scroll position and the open
rows go with the entity. Coming back makes a new `ChatPane` with an empty
`Chat`, sends `OPEN`, and waits for a snapshot of up to 200 items and 1 MiB:
one round trip plus the transfer plus laying out the conversation again,
behind "Opening the chat…".

**Design (a): keep panes alive.** A bounded, most-recently-used set of
`Entity<ChatPane>` per window, keyed as `OpenChats` is (profile, server,
pane). `apply_chat` asks it for the pane instead of making one. A kept pane
stays subscribed, so it is current when it is shown again, with its scroll
position and open rows. It is let go when: it falls out of the set (eight;
then `released` closes it as today); its tmux pane is gone or is no longer a
chat (from the topology the surface already reads); its window closes. A
kept pane whose link dropped shows what it holds at once and opens again by
`follow_link`, as a visible pane does; while what it holds may be behind,
the pane says "Updating…" (mock S7) instead of the blank opening state.
Cost: the host keeps sending events for up to eight chats per window; deltas
are already joined for 50 ms.

Not proposed: an `OPEN` that quotes the client's revision so the host can
answer "unchanged". The host keeps no log of changes to send a tail from, and
a kept subscription already covers the case.

**Root cause (b): confirmed state only.** `model_pill`, `effort_pill`,
`permission_pill` and `speed_toggle` read `chat.state.options`;
`send_button` reads `chat.state.turn`. A choice is drawn after the request
has reached the host, the agent has answered the host (`Claude::set_options`
asks `set_model`, then `initialize` and `get_settings`), and the state event
has come back. Sending is already optimistic (the dimmed bubble).

**Design (b): an overlay.** `ChatPane.wanted: Option<ChatOptions>` is laid
over the state's options when drawing. It is dropped when the state agrees
with it, when the request is refused (the notice says why), or three seconds
after the request was answered (the agent settled on something else, as with
fast mode on an account without it). `ChatPane.stopping` does the same for
Stop: set by `interrupt`, cleared when the turn leaves WORKING, on a
refusal, or after ten seconds.

**Smaller, optional.**

- *Edit in one round trip.* Editing sends `REWIND`, waits, then `SEND`: two
  round trips, and a half-done state the pane has to stay alive for
  (`submit`). `SEND` with `item_id` set could mean "rewind to it, then
  send": one request, nothing in between. Additive on the wire (§4).
- *File-link probes.* `pump_probes` asks about one inline-code path at a
  time, a round trip each, and every answer that finds a file clears
  `shown` and remeasures the whole list. Ask for up to six at once (the
  bridge matches answers by request id) and remeasure only the items that
  name the path.

**Root cause (c).** §2.

**Tests.** A pane unmounted and mounted again sends no `OPEN` and keeps its
scroll position; the ninth chat closes the first; a pane whose tmux pane
went is let go. The pill shows a choice before the answer and snaps back on
a refusal; Stop likewise.

## 4. Wire changes, all additive

One commit, before the packages that use them. `PROTOCOL_MAJOR` stays 5.

| Change | Used by | Previous phone, new host | New phone, previous host |
| --- | --- | --- | --- |
| `chat.proto`: `message ChatAttachment { path, name, media_type, size_bytes }`; `ChatRequest.attachments` (SEND); `ChatItem.attachments` (USER) | 3.1 | Never sends them; ignores the item field and shows the text. A message of attachments alone is an empty bubble, and editing a message from the phone resends its text without its attachments. Neither breaks the phone: no bump | The phone is not changed to send them |
| `chat.proto`: `CHAT_ACTION_FORK = 11`, taking `item_id`, answering `forked_pane_id` | 3.8 | Never sends it | Not sent by the phone |
| `envelope.proto`: `GitRequest.summary_base` (enum, zero = default branch) | 3.4 | The phone does not ask for summaries | Same |
| `chat.proto`: `ChatBackgroundTask`, `ChatTaskKind`; `ChatState.background = 11`, `ChatState.compacting = 12`; `ChatTool` fields 11–15 (`agent_type`, `activity`, `tool_uses`, `tokens`, `background`); `CHAT_ACTION_STOP_TASK = 12` with `task_id` | 3.14 | Ignores the fields and never sends the action; draws the chat as it does today | Reads empty values from a host that does not set them: no chip, no figures. Not sent by the phone |
| Optional: SEND with `item_id` rewinds first | 3.12 | Sends REWIND then SEND as before; the host keeps both | Not sent by the phone |

Desktop and host change together, so the desktop's old paths are deleted
where a new one replaces them (the optional SEND-with-rewind would delete
the desktop's REWIND-then-SEND; the phone's REWIND stays on the wire).

## 5. Provider facts

| Fact | State |
| --- | --- |
| Claude Code headless takes an `image` content block in a `user` line | Verified, 2.1.326, one live turn |
| Claude Code print mode forks cut at a message (`--fork-session` with `--resume-session-at`) | Verified, 2.1.326; source session untouched |
| Claude Code's `initialize` names the default's real model (`resolvedModel`, `description`) | Verified, 2.1.326 |
| Codex `turn/start` input takes `localImage` and `mention` | Verified, 0.169.0, live (17 Nov): the model sees a `localImage` without a tool call; the echoed user item carries it back unchanged. A `mention` is accepted but the server does not read the file in; the host sends paths in the text instead |
| Codex `thread/fork` takes `lastTurnId` | Verified, 0.169.0, live (17 Nov): the fork holds the turns up to that one and keeps the source's turn and item ids; `lastTurnId` is the server's turn id (a client message id is refused, "turn not found"); without it the whole thread is copied |
| Claude Code headless sends `background_tasks_changed`, `task_started`, `task_progress`, `task_updated`, `task_notification`, `thinking_tokens` | Verified, 2.1.326, one live turn with a background command and a subagent |
| Claude Code sends `system/status { status: "compacting" }`, then `status: null` and `compact_boundary` with `pre_tokens` and `post_tokens` | Verified, 2.1.326, one live `/compact` |
| Claude Code stops a background task on the control request `stop_task { task_id }` | Verified, 2.1.326, live |
| Codex `commandExecution.source = unifiedExecStartup` | Verified, 0.169.0, live (17 Nov): a background command outlives its turn; the turn completes with no `item/completed` for it, and one arrives late, with the old turn's id, when the process ends. `thread/read` leaves a running one out. Listing and stopping (`thread/backgroundTerminals/*`) need the `experimentalApi` capability, which the host does not send: no stop on Codex |
| Codex `contextCompaction` item, `collabAgentToolCall` with `agentsStates` | In the 0.169.0 schema; not run live |
| Claude Code downsizes an oversized inline image | Not known; avoided by the 5 MB rule |

## 6. Work packages

Each is one agent's job, in this order where they share files. `chat/pane.rs`,
`chat/view.rs` and `chat/composer.rs` are shared by C, D, E, F and G: those run
one after another, not side by side.

| Package | Contents | Needs first | Files it owns |
| --- | --- | --- | --- |
| W — wire | §4 in one commit, generated phone code, round-trip tests | Mock approved for S1, S3, S6 | `crates/protocol`, `apps/mobile/src/protocol/gen`, `crates/desktop-core/src/connection/{chat,git}.rs` |
| A — card and names | 3.2, 3.3, 3.5, then 3.4 | Mock S1, S2; W for 3.4 | `chat/details.rs`, `app.rs` (assets), `git/{panel,client,model}.rs`, host `git/branch.rs`, host `chat/claude.rs` (`initialized` only) |
| B — keep-alive and overlay | 3.12 (a) and (b) | Mock S7 | `chat/pane.rs` (lifecycle), `chat/mod.rs`, `terminal/surface.rs` (`apply_chat`), `chat/composer.rs` (pills, Send) |
| C — timeline | 3.6, 3.7, the buttons of 3.8 | Mock S3, S4, S5; after B | new `chat/timeline.rs`, `chat/view.rs`, `chat/pane.rs` (rows), host `chat/claude.rs` (subagent count) |
| D — attachments | 3.1 | Mock S6; W; after C | `terminal/integration/upload.rs`, new `chat/attach.rs`, `chat/composer.rs`, host `chat/{mod,claude,codex,provider}.rs` |
| E — fork | fork of 3.8 | Mock S3, D6; W; after C | host `chat/{convert,claude,codex}.rs`, `requests/chat_dispatch.rs`, `chat/pane.rs` (one action) |
| G — tasks, subagents, compacting | 3.14 | Mock S9, D15–D18; W; after C (it builds on the status line and the groups) | host `chat/{claude,codex,mod}.rs` (new arms, state fields), `chat/view.rs` and `chat/timeline.rs` (subagent rows, status line), `chat/composer.rs` (chip) |
| F — colours and optional | 3.10; the two optional items of 3.12 | Mock S8 | `theme/`, `chat/pane.rs` (probes) |

A and B can start together; A's host work and B's desktop work share no
file. Each package ends with: `cargo fmt`, `cargo clippy` and the tests of
the crates it touched, `cargo deny check licenses` when `Cargo.lock`
changed (none should), and for W `pnpm --filter mobile protocol:check`.

A final pass against a real host over a throttled link (the owner's 1 s
case) checks 3.12 by feel and by the perf log: reopening a kept chat, a
model change, Stop.

## 7. The owner's answers (16 Nov)

Given through the mock's answer form. Sections S1 to S9: all fine as drawn.

| # | Question | Answer |
| --- | --- | --- |
| D1 | Host icon | `server` |
| D2 | Commit icon, changes icon | `git-commit-horizontal`, `file-diff` |
| D3 | Changes rows | "Uncommitted" and "Branch vs main" |
| D4 | Default model label | "Opus 5.5 (default)"; the plain "Opus 5.5" entry stays |
| D5 | Agent message buttons | Under the last text of each reply |
| D6 | Fork cut | The whole reply |
| D7 | Which runs collapse | 3 or more |
| D8 | Summary after the turn | None |
| D9 | Attachments | Images as images, other files as their host path |
| D10 | Esc | One press |
| D11 | Working mark | Still, ticking seconds |
| D12 | A fork from a reply | Listed in History |
| D13 | Collapsed line | Counts |
| D14 | Colours | C1 only |
| D15 | Background tasks | Chip on the composer's top edge |
| D16 | Running subagents | One row each |
| D17 | Compaction notice | With the size before |
| D18 | Stop a background task | Wanted, not required: "better to have it". It is cheap on Claude Code (verified live), so each row gets a stop button as S9 draws it; Codex rows have none. |

All are the recommended option, so the mock as drawn is the specification. C2 and C3 of §3.10 are not built.

## 8. State (17 Nov)

All eight packages are built and committed on `gpui/chat`, in the order W,
A, B, C, G, D, E, F (the hashes are in §0). The two optional items of §3.12
and C2/C3 of §3.10 are not built.

Reviewed by four independent readers (host code, desktop code,
invariants, a render of the real pane against the mock) and once more
after the fixes; then driven in the real app against a real Claude Code
chat on a separate dev daemon. What was found and accepted is fixed in the
commits after `5ff96360`. Two things only the real run showed: with Stop
hooks set, the entry before a user's message is a hook's summary, so a
fork or a rewind must be cut at the nearest message before (§3.8); and
the background subagent below.

Known and left as they are:

- **Codex.** A background command is listed while it runs, with no stop
  button: listing and stopping need Codex's experimental API. One that is
  running when a chat is reopened is not shown (`thread/read` leaves it
  out). Compaction and subagent items are built from the schema and were
  not run live.
- **Claude, background subagent.** Claude Code runs a subagent in the
  background by default (seen live, 2.1.326): its tool call returns at
  once and its report comes back later as a line marked `isSynthetic`. The
  row stays running with its figures until the task ends, between turns
  too, and the report's line is not drawn. A chat reopened while one runs
  shows the row as done, as the record has it. The chip is empty after a
  host restart until the agent next reports its list.
- **Light theme.** Dim ink is 4.62:1 on the pane, but 4.39:1 on raised
  surfaces and 3.77–3.95:1 on washes. Open with the owner: darken light
  `chromeDim` everywhere, or add an ink for dim text on raised surfaces.
- **Mac.** Cmd+C in the chat (problem 13) is covered by tests, not checked
  on a Mac.
