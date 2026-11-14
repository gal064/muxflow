# Chat mode and T064 — build plan

Branch `gpui/chat`, worktree `../muxflow-chat`. The reviewed design
and mocks are in [chat-mock.html](chat-mock.html); this file is the
engineering plan that implements it. Where the two disagree, the mock's
"Decisions" table wins and this file is wrong.

## 1. What is being built

| Piece | Summary |
| --- | --- |
| Chat mode | One host-wide mode, Terminal or Chat (default Terminal), set from the desktop. In Chat mode an agent started in a terminal (`claude`, `codex`, any alias) becomes a chat in the same tmux pane. No per-session switching. Changing the mode affects agents started afterwards only. pi stays a terminal (T066). |
| Chat view | Per pane, inside the existing pane tiling. Timeline, tool rows, question card, Auto-mode fallback card, composer with slash/skill autocomplete, edit-and-resend (rewind), model, effort, Full/Auto, Standard/Fast, context meter, file links. |
| `/btw`, `/side` | Muxflow-owned. Fork the session (existing Branch), open it in a split, send the question. Same permissions as the source. Titled `<name> (fork)`. Hidden from History. |
| Details card | Chat panes only, floating top right, hidden when the chat area is under 940px (Details button opens it as a popover). Host and folder, branch menu, Commit…, Changes vs origin/main, context. |
| Git view | Git panel stays the file list, with a scope switch (Uncommitted / vs origin/main). Diffs open in one reused tab, one file at a time, hunks only, hunks collapsible. |
| History | Host records sessions it sees (cap 1,000, JSON). Overlay from the Agents header, command palette and a shortcut. Title/workspace/provider fuzzy search on the client. Snoozed group first. Enter focuses a live session or resumes it in the same-named workspace, creating that workspace in the session's folder if it is gone. |
| Snooze | Closes the session, stores a wake time on the host. At wake it reappears as a placeholder row marked "Woke" with a notification; click resumes. Intervals configurable in desktop Settings. Refused while working or blocked. |
| Phone | Chat screen, voice mode inside chat, History, Snooze. The Voice screen stays for terminal sessions. The mode is shown read-only. |

Not in this work: terminal link fixes, pi chat (T066), open in editor (T067),
cite into chat (T068), idle release, CPU/RAM monitor, per-session change
tracking, a shared Codex server.

## 2. Rules that apply to every stream

- Repo rules in `CLAUDE.md` hold: no compatibility arms, desktop and host change
  together, MIT only, do not read Zed's GPL crates, kill dev daemons when done.
- `PROTOCOL_MAJOR` is bumped once, in stream A. Nobody else bumps it.
- Never read, copy or store an agent's credentials. Run the installed `claude`
  and `codex` binaries unmodified. Do not call a vendor API directly.
- No screen scraping and no keystroke driving for chat. Chat data comes from the
  provider's structured interface; history comes from the provider.
- Everything on the wire is bounded: tool output previews ≤ 16 KiB, a snapshot
  carries the newest ≤ 200 items, a frame stays far under `MAX_FRAME_BYTES`.
- Provider quirks stay inside the provider module. Nothing above the
  `ChatProvider` trait names Claude or Codex.
- A vendor format that cannot be read fails soft: the chat still works and says
  "Earlier messages are not available".
- Prefer the simplest correct thing. No speculative options or settings.

## 3. Host architecture

```
tmux pane ── muxflow-host chat-pipe ── claude -p (stream-json)      ┐
tmux pane ── codex app-server --listen unix://…                      ├─ unix socket ─ ChatRuntime (daemon) ─ wire ─ desktop / phone
                                                                     ┘
```

- **`ChatRuntime`** (`apps/host/src/service/chat/mod.rs`), process-global like
  `AgentRuntime`. Owns one `ChatSession` per chat pane, the per-pane timeline
  (bounded), subscriptions per connection, and reconnect.
- **`ChatProvider` trait** (`chat/provider.rs`), one impl per adapter, registered
  beside the existing adapter table (`service/agents/adapters.rs`):
  - `adapter_id()`, `capabilities()`
  - `pane_command(launch, socket) -> argv` — what runs in the pane
  - `connect(stream, launch, sink) -> Box<dyn ChatSession>` — translates provider
    messages into chat events and chat commands (send, interrupt, answer, rewind,
    set options) into provider messages
  - `load_history(launch) -> Vec<ChatItem>` — earlier messages for a resumed session
- **`chat-pipe`** (`apps/host/src/chat_pipe.rs`, subcommand `muxflow-host
  chat-pipe -- <argv>`): spawns the child, bridges its stdin/stdout to a unix
  socket, keeps a 4 MiB ring of output while no daemon is connected and replays
  it on connect, prints one line to the pane ("Muxflow chat session"). It knows
  nothing about any protocol, so an old pipe and a new daemon cannot disagree.
- **Pane tag**: pane options `@muxflow-chat` (adapter id) and
  `@muxflow-chat-socket`. Added to `PANE_FORMAT`; surfaces as
  `Pane.chat_adapter_id`. A restarted daemon rediscovers chat panes from the tag
  and reconnects.
- **Sockets** live in the runtime dir: `/tmp/muxflow-<uid>/chat/`.
- **Conversion at start** (`chat/convert.rs`), only in Chat mode, only for
  adapters with a provider, only for a pane without the tag:
  1. Trigger: the agent's `SessionStart` hook (keep its `source` field), or
     process detection when hooks are not wired.
  2. Resume id: the hook's session id when `source` is `resume`, or a resume id
     parsed from the process argv; otherwise a fresh session.
  3. Carry over from `/proc/<pid>`: cwd, environment, and the launch flags for
     model, permission and extra directories. Drop the rest.
  4. `tmux respawn-pane -k` in the same pane with
     `sh -c '<pane_command>; exec "${SHELL:-/bin/sh}" -i'`, then set the tag.
  5. The headless child inherits the pane, so its hooks keep reporting to the
     same pane. A tagged pane is never converted again.
- **Permissions**: `ChatPermissionMode` is Full or Auto. From launch flags
  (bypass/skip-permissions ⇒ Full), otherwise Auto. Mappings already exist in
  `plugins/automations/launch.rs` (`--permission-mode auto`,
  `--approve-for-me`). If the provider still asks, surface a
  `ChatPendingRequest` of kind PERMISSION with Allow / Deny.
- **Status**: lifecycle still comes from hooks through `AgentRuntime`. A pending
  chat request is also reported to `AgentRuntime` as blocked through the same
  ingest path, so questions show as blocked even where no hook fires.
- **Agent mode**: stored in the host state dir (`settings.json`), exposed as
  `AgentSnapshot.agent_mode`, changed with `CHAT_ACTION_SET_MODE`.

### Provider notes (verified locally unless marked)

Claude Code 2.1.325, `claude -p --input-format stream-json --output-format
stream-json --verbose --include-partial-messages --replay-user-messages
--permission-prompts host`:

- `control_request {subtype:"initialize"}` returns commands (78 here, with
  description and argument hint), models with `supportedEffortLevels`,
  `supportsFastMode`, `current_permission_mode`, `fast_mode_state`. No prompt,
  no usage.
- Other control subtypes present in the binary: `interrupt`, `set_model`,
  `set_permission_mode`, `get_context_usage`, `rewind_files`, `side_question`.
- Hooks fire in headless mode.
- Rewind: restart the child with `--resume <id> --resume-session-at <message
  uuid> --fork-session` (flag exists; **not yet run end to end**).
- Fast mode needs the session to opt in through flag settings
  (`--settings '{"fastMode":true}'`); **not yet run**.
- History: the session's JSONL under `~/.claude/projects/`; same message shapes
  as the stream. **Undocumented — fail soft.**
- The official Python SDK is MIT and is the reference for the control protocol.
  Do not add a third-party Claude crate.

Codex 0.169.0, `codex app-server --listen unix://PATH` (JSON-RPC; schema via
`codex app-server generate-json-schema --out DIR`):

- `thread/start`, `thread/resume`, `thread/fork`, `thread/revert
  {threadId, beforeTurnId}`, `thread/read`, `thread/turns/list`,
  `turn/start {input, model, effort, serviceTier, approvalPolicy, sandboxPolicy,
  approvalsReviewer}`, `turn/interrupt`, `turn/steer`, `model/list`,
  `skills/list`.
- Server requests: `item/commandExecution/requestApproval`,
  `item/fileChange/requestApproval`, `item/permissions/requestApproval`,
  `item/tool/requestUserInput`.
- Notifications: `item/*`, `turn/*`, `thread/tokenUsage/updated`,
  `turn/diff/updated`, `skills/changed`.
- One server per chat pane. Measured: about 270 MB per chat; a shared server
  would save about 100 MB per chat. Keep thread addressing explicit so sharing
  later only touches this provider.
- **To verify**: hooks from a per-pane server are attributed to that pane.

## 4. Wire contract (stream A owns it)

New files `crates/protocol/proto/chat.proto` and `history.proto`, imported by
`envelope.proto` the way `automation.proto` is.

`envelope.proto`:

- `OPERATION_CHAT = 60`, `OPERATION_SESSION_HISTORY = 61`
- `Request.chat = 27`, `Request.history = 28`
- `Response.chat = 16`, `Response.history = 17`
- `EVENT_KIND_CHAT = 24`, `EVENT_KIND_SESSION_HISTORY = 25`
- `HostEvent.chat = 15`, `HostEvent.history = 16`
- `Pane.chat_adapter_id = 15` (empty for a terminal pane)
- `AgentSnapshot.agent_mode = 10`

`chat.proto` (names final, field numbers assigned in order):

```proto
enum ChatAgentMode { UNSPECIFIED; TERMINAL; CHAT }
enum ChatAction { UNSPECIFIED; OPEN; CLOSE; SEND; INTERRUPT; ANSWER; REWIND;
                  SET_OPTIONS; LOAD_OLDER; SIDE_FORK; SET_MODE }
enum ChatPermissionMode { UNSPECIFIED; AUTO; FULL }
enum ChatSpeed { UNSPECIFIED; STANDARD; FAST }
enum ChatTurnState { UNSPECIFIED; IDLE; WORKING; WAITING }
enum ChatConnection { UNSPECIFIED; CONNECTING; READY; ENDED; FAILED }
enum ChatItemKind { UNSPECIFIED; USER; ASSISTANT; REASONING; TOOL; NOTICE }
enum ChatToolKind { UNSPECIFIED; COMMAND; READ; EDIT; SEARCH; WEB; MCP; SUBAGENT; OTHER }
enum ChatToolStatus { UNSPECIFIED; RUNNING; OK; FAILED }
enum ChatRequestKind { UNSPECIFIED; PERMISSION; QUESTION }
enum ChatCommandKind { UNSPECIFIED; COMMAND; SKILL; MUXFLOW }

message ChatOptions { string model; string effort; ChatPermissionMode permission; ChatSpeed speed; }
message ChatTool { ChatToolKind kind; string title; string detail; ChatToolStatus status;
                   string result_summary; string path; uint32 added; uint32 removed;
                   string output_preview; bool output_truncated; }
message ChatItem { string id; ChatItemKind kind; string text; bool streaming; ChatTool tool;
                   int64 at_unix_millis; bool rewindable; }
message ChatOption { string id; string label; string description; }
message ChatQuestion { string id; string header; string prompt; repeated ChatOption options;
                       bool multi_select; bool allow_other; }
message ChatPendingRequest { string request_id; ChatRequestKind kind; string title; string detail;
                             repeated ChatQuestion questions; }
message ChatQuestionAnswer { string question_id; repeated string option_ids; string other_text; }
message ChatAnswer { bool allow; repeated ChatQuestionAnswer answers; bool skipped; }
message ChatCommand { string name; string description; string argument_hint; ChatCommandKind kind; }
message ChatModel { string id; string label; repeated string efforts; bool supports_fast; }
message ChatCapabilities { bool rewind; bool send_mid_turn; bool model; bool effort; bool speed;
                           bool permission; bool fork; bool questions; }
message ChatCatalog { ChatCapabilities capabilities; repeated ChatModel models; repeated ChatCommand commands; }
message ChatUsage { uint64 used_tokens; uint64 window_tokens; }
message ChatState { string pane_id; string adapter_id; string native_session_id; ChatTurnState turn;
                    ChatConnection connection; string failure; ChatOptions options; ChatUsage usage;
                    ChatPendingRequest pending; bool history_complete; }
message ChatSnapshot { uint64 revision; ChatState state; ChatCatalog catalog;
                       repeated ChatItem items; string older_cursor; }

message ChatRequest { ChatAction action; string pane_id; string expected_server_identity;
                      string text; string client_message_id; string request_id; ChatAnswer answer;
                      string item_id; string older_cursor; ChatOptions options; ChatAgentMode mode; }
message ChatResponse { repeated ChatItem older; string older_cursor;
                       string forked_pane_id; ChatAgentMode mode; uint64 revision; }

message ChatItemDelta { string item_id; string text_append; }
message ChatTruncate { string after_item_id; }   // rewind: drop everything after this item
message ChatEvent { string pane_id; uint64 revision;
                    oneof change { ChatItem item; ChatItemDelta delta; ChatState state;
                                   ChatCatalog catalog; ChatTruncate truncate;
                                   ChatSnapshot snapshot; ChatItem updated; } }
```

Semantics:

- `OPEN` subscribes the connection to one pane; the snapshot arrives as a chat
  event ahead of the answer. Chat events go only to subscribed connections.
- `revision` increases by one per change per pane. A client that sees a gap
  re-sends `OPEN`.
- `SEND` while `turn` is WORKING is delivered mid-turn when the provider's
  capabilities say so; otherwise the host refuses with a display message.
- `REWIND` names a `rewindable` USER item; the host emits `truncate`, then the
  edited message is a normal `SEND`.
- `SIDE_FORK` forks through the existing branch action with split placement,
  marks the fork as a side session, and sends `text` as its first message.
- `SET_MODE` writes the host agent mode; the next agent snapshot carries it.

`history.proto`:

```proto
enum HistoryAction { UNSPECIFIED; LIST; RESUME; SNOOZE; WAKE; }
message HistoryEntry { string adapter_id; string native_session_id; string title; string workspace_name;
                       string cwd; int64 last_active_at_unix_millis; bool running; string agent_id;
                       int64 snoozed_until_unix_millis; bool woke; bool chat; }
message HistoryRequest { HistoryAction action; string adapter_id; string native_session_id;
                         int64 snooze_until_unix_millis; string expected_server_identity;
                         uint64 expected_topology_generation; }
message HistoryResponse { uint64 generation; repeated HistoryEntry entries;
                          string session_id; string window_id; string pane_id; }
message HistoryEvent { uint64 generation; repeated HistoryEntry woke; }
```

Clients `LIST` on open and after any `HistoryEvent`; search is client-side.
Side forks are never listed.

### As built by stream A

The messages above are in the tree; the field numbers and comments are in
`chat.proto` and `history.proto`. What §4 left open, and what changed:

- **Open, and how a client follows a chat.** The snapshot is a chat event, not
  part of the answer to `OPEN`. Both clients hand a response and an event to
  their consumers on different paths, so a snapshot in the response could
  reach a consumer after the events that follow it. As an event it is in order
  with them by construction. A client keeps one revision per open pane:
  - a `snapshot` replaces what it holds and sets the revision;
  - any other event applies when its revision is one more, is ignored when it
    is not newer, and otherwise means a change was lost: send `OPEN` again;
  - events for a pane it holds no snapshot of are ignored;
  - after a `RESYNC_REQUIRED` event or a reconnect it sends `OPEN` again for
    every chat it has open. The desktop bridge drops the events that arrive
    while it resyncs, and a dropped last event of a turn leaves no later gap
    to notice.

  `OPEN` on a chat that is already open only brings a fresh snapshot. A
  connection may open several panes.
- **New items and updates.** `item` is always a new item, after every other.
  A change to an item already in the timeline is `updated`, which keeps the
  item's place; a client that does not hold the item (it lies before what it
  has loaded) ignores it. A provider says which it is publishing. An update
  to an item the host's timeline no longer holds, because it was rewound or
  trimmed away, is dropped rather than brought back as the newest item. An
  item never changes kind: one published under a held id with another kind is
  dropped, so a provider's item cannot replace a message whose id a client
  chose.
- **Older pages** come back in the response, on a different path from events,
  so `ChatResponse.revision` says when the page was read. A client:
  - drops a page that answers a request made before its current snapshot;
  - keeps a page aside until its own revision for the pane has reached the
    page's, since the events up to there are already in the page;
  - asks for the page again if, past the page's revision, it ignored an
    `updated` or a `delta` for an item it did not hold.
- **Pending.** `ChatState.pending` is set only while the turn is WAITING; the
  runtime clears it from any other state a provider publishes.
- **Rewind.** `REWIND` names the user message being edited, so that message goes
  too: `ChatTruncate.after_item_id` is the item *before* it, and is empty when
  the first message is rewound (drop everything). The runtime works out
  `after_item_id` and the session publishes the truncate, at the point in its
  own stream where the rewind takes effect. A client that does not
  hold `after_item_id`, because it has not loaded that far back, sends `OPEN`
  again.
- **Send.** `client_message_id` becomes the id of the USER item, so a client
  can match the item to what it sent. Empty lets the host choose; an id already
  in the timeline is refused with `chat_message_duplicate`, which tells a
  client that sent a message again, having lost the answer, that the first one
  arrived. Clients send random ids.
- **Options.** In `SET_OPTIONS` an empty string or an unspecified enum keeps the
  current value. The runtime does the merge and checks what is asked for
  against the pane's catalog, so a provider is handed the whole options: what
  is asked for must be something the provider's capabilities say it can
  change, a model or an effort asked for must be listed, and fast must be
  supported by the model. What a kept effort or speed becomes when a new model
  lacks it is the provider's to say (the scripted one takes the highest effort
  and standard speed). While the current model is one the catalog does not
  list, only the permission mode can change.
- **Answer.** `ANSWER` without an `answer` is refused; it is not a denial.
- **One command at a time per pane.** A command is checked and handed to the
  provider under the pane's session lock, so two connections with the same
  chat open cannot interleave: the second is checked against what the first
  left.
- **A full event queue** costs the connection the same resync a saturated
  broadcast does, instead of dropping a chat event silently. An `OPEN` whose
  snapshot cannot be queued is refused with `chat_queue_full` instead, and the
  client sends it again. A resync is the heaviest recovery there is (the phone
  reconnects), and chat events are neither coalesced nor paced the way
  terminal output is, whose flood can also be what fills the queue. Left open
  for stream B, whose providers stream a delta per token: batch deltas before
  publishing them, and consider answering a full queue with a fresh snapshot
  of the one pane once there is room, instead of a resync of the connection.
- **Mode.** `SET_MODE` answers its caller and tells nobody else: other
  connections read the new mode from their next agent snapshot. Streams C and
  F need that to be soon enough, or an event.
- **Panes and servers.** A chat belongs to a pane of one tmux server: the
  runtime keys it by `expected_server_identity` and pane id. When the
  connection has a topology, a pane action is refused with
  `chat_server_changed` if that is not the server it last saw and with
  `chat_unavailable` if that server has no such pane; `CLOSE` of a well-formed
  pane id is never refused. A connection with no topology is taken at its
  word, and its pane is untagged: that is one that never asked for it, and
  also one that has just launched an agent, until the next topology reaches
  it. Stream B, whose
  providers go by the pane's tag, has to cover that window (the scripted
  provider claims untagged panes, so it does not show today). Each chat
  request with a topology also lets go of the chats that connection has open
  on panes the topology no longer has, and on other servers: the connection
  stops hearing from them, and a chat nobody else has open is forgotten, which
  drops its provider session. So a client sends `CLOSE` when a chat's pane
  goes. No connection's view is trusted to say that a chat it does not have
  open is gone; the host's own fresh look at tmux, each time the topology
  actor reconciles one, forgets every chat nobody has open whose pane is not
  a live pane of that server. Such a look can be older than a pane, so a
  chat it forgets between its start and its first use is started again; one
  forgotten later, while nobody has it open, loads its history again at the
  next request.
- **Bounds**, all enforced in `ChatRuntime` so no provider has to:
  - tool output preview 16 KiB; a tool's or a pending request's `detail`
    16 KiB; a tool's `path` 4 KiB; item text 256 KiB; every other string
    1 KiB;
  - an item id 128 bytes: an item with a longer or an empty id is dropped;
  - a pending request 8 questions of 16 options; a catalog 64 models of 16
    efforts and 256 commands;
  - a snapshot or an older page 200 items and 1 MiB;
  - a pane's timeline 2,000 items and 32 MiB of text. Past either the oldest
    items are dropped and `history_complete` turns false;
  - inbound: a message 256 KiB, a `client_message_id` 128 bytes, an answer
    64 KiB, a pane id 11 bytes, a server identity 1 KiB.

  The older cursor is the id of the first item of the page it follows.
- **Refusals** carry an `error_code`: `chat_invalid_request`, `chat_unavailable`
  (the pane has no chat), `chat_not_ready`, `chat_server_changed`,
  `chat_turn_busy`, `chat_request_not_pending`, `chat_item_not_rewindable`,
  `chat_message_too_large`, `chat_message_duplicate`, `chat_cursor_invalid`,
  `chat_queue_full`,
  `chat_mode_not_saved`, `chat_side_fork_unavailable` (until stream B wires it),
  `history_invalid_request`, `history_unavailable` (every action but `LIST`,
  until stream E).
- **`Pane.chat_adapter_id`** is carried by `tmux_control::Pane`, which is what
  the host discovers and what the desktop receives, so streams B and C fill and
  read one field. Nothing tags a pane yet: it is empty, except that while the
  scripted provider is registered every untagged pane reads `scripted`, so the
  clients have a chat pane to open.
- **Provider trait.** `ChatProvider` has `adapter_id`, `capabilities`,
  `load_history` and `connect(pane_id, sink)`; `ChatSession` takes one
  `SessionCommand`. `pane_command` and the socket `connect` reads from arrive
  with the pane process, in stream B. Chat requests run inline on their
  connection's ordered lane, so `load_history`, `connect` and `command` must
  not wait on anything slow: a provider that would hands the work to its own
  thread, or stream B moves the first `OPEN` of a pane off the lane. The ids a
  provider gives its items and the `client_message_id`s clients choose are one
  namespace. Clients send random ids, and the kind rule above keeps a
  provider's item from taking a user message's place if two ever meet.
- **Desktop frames.** Until streams C and E read them, both desktops let frame
  kinds 20 and 21 pass: the GPUI app's two decoders (the connection UI's and
  the terminal engine's, which owns the sequence) ignore them, and the Tauri
  decoder treats them as protocol progress. The Tauri desktop keeps that arm;
  it gets no chat.
- **Scripted provider.** Each pane opens on a finished 233-item transcript.
  Messages start two turns in rotation: one stops on a question (two questions:
  one with an "other" answer, one multi-select), the next on a permission
  request. A step runs every 120 ms. It says it can `fork` so the clients
  have the control to build, though `SIDE_FORK` is refused for every provider
  until stream B wires it.
- **Desktop core** carries chat and history as the protocol's own types, not
  JSON: `connection::chat::chat_request`, `connection::history::history_request`,
  and event frames 20 (chat, labelled with the pane) and 21 (history) whose
  payloads `chat::decode_event` and `history::decode_event` read. The agent
  snapshot JSON gains `agentMode` (`"terminal"` or `"chat"`).

Git additions belong to stream D alone (branch summary against the default
branch, branch list, checkout, and a diff target for the default-branch scope).
The concise diff renders `GitDiff.patch`, which the host already sends and the
desktop currently drops, so hunk indices match the host's.

## 5. Streams

Each stream runs its own implement-and-review cycle in its own worktree
(`../muxflow-chat-<stream>`, branched from `gpui/chat` after A is
merged) and merges back into `gpui/chat`. One manual QA at the end.

### A — Contract and runtime skeleton (lands first, alone)

- The proto in §4, `PROTOCOL_MAJOR` bump, regenerated mobile protocol code and
  contract.
- Host: operation policy rows, dispatch, `ChatRuntime` with subscribe / publish /
  revision, the `ChatProvider` trait, and a **scripted provider** (adapter id
  `scripted`, enabled only by `MUXFLOW_CHAT_SCRIPTED=1`) that replays a fixed
  session covering every item kind, a question, a permission fallback, a rewind
  and usage. Streams C and F build against it; stream B reuses it as the shared
  provider test harness.
- Desktop core and mobile: request builders and event decoding only, no UI.
- Done when: `cargo test --workspace`, clippy, fmt, `cargo deny check licenses`,
  `pnpm mobile:check`, `pnpm --filter @muxflow/mobile protocol:check` pass, and a
  host test drives OPEN → SEND → ANSWER → REWIND against the scripted provider.

### B — Host chat

- `chat-pipe`, the Claude and Codex providers, conversion at start, pane tag,
  reconnect after a daemon restart, agent mode storage, pending request ⇒
  blocked.
- Verify the three open provider facts in §3 first and record the results here.
- Shared provider tests: every provider passes the same assertions against
  recorded sessions (fixtures under `apps/host/src/service/chat/fixtures/`).
- Done when: a real `claude` and a real `codex` each run a turn, a question, an
  interrupt and a rewind through the wire against a private tmux socket, and a
  daemon restart mid-turn reconnects.

### C — Desktop chat

- `apps/desktop-gpui/src/chat/`: reducer, timeline, tool rows, cards, composer,
  autocomplete, rewind, pills (model, effort, Full/Auto, Standard/Fast), context
  meter, file links (Markdown links and inline code paths that resolve on the
  host; show the file name), `/btw` and `/side`.
- Render per pane inside `TerminalSurface`: a pane with `chat_adapter_id` shows
  the chat instead of the terminal grid and does not attach a terminal stream.
- Settings: agent mode (Terminal / Chat).
- Reuse: Kit `TextView` Markdown, `TextareaState`, `fuzzy_match`, context menus,
  `WorkingIcon`. Follow the accessibility rule in `ui/mod.rs`.
- Done when: every piece in the mock works against the scripted provider, with
  view tests in the style of the existing `tests.rs` files.

### D — Git view and details card

- Host: branch summary against the default branch, branch list, checkout, and
  the default-branch diff scope.
- Desktop: hunks-only diff from `GitDiff.patch` in one reused tab (resolves
  T041), collapsible hunks, "Show whole file", scope switch in the Git panel.
- Details card (`chat/details.rs`) once C's pane view has merged.
- Done when: staging and discarding a hunk still act on the hunk shown.

### E — History and Snooze

- Host: `service/history/` store (`sessions.json`, cap 1,000), fed from
  `AgentRuntime` when a native session id is learned, a title changes, or an
  agent retires; snooze records; the wake check on the daemon's 2 s timer.
- Desktop: History overlay (reuse `QuickOpen`'s pattern), Snoozed group, woke
  placeholder rows in the Agents list, Snooze in the row and tab menus, snooze
  intervals in Settings. All forks titled `<name> (fork)`.
- Done when: a closed session resumes from History into the right workspace, and
  a one-minute snooze wakes as a placeholder after a daemon restart.

### F — Phone

- `apps/mobile/src/features/chat/` and `app/chat/[paneId].tsx`: timeline, cards,
  composer with autocomplete, pills, details sheet. Reuse `VoiceMarkdown` and
  the Voice screen's bubbles.
- Voice mode inside chat using the existing transcribe and speak operations.
- History screen, Snooze in the long-press menu, read-only agent mode in
  Settings.
- Done when: `pnpm mobile:check` and `pnpm mobile:test` pass and every phone mock
  works against the scripted provider.

#### As built by stream F

No wire change. What the phone does, and where it departs from the mock:

- **Following a chat.** `features/chat/ChatClient.ts` keeps one pane per
  open chat screen (screens `acquire` it; the last to let go sends `CLOSE`)
  and applies the rules in "As built by stream A" through the pure reducer in
  `chatModel.ts`. A lost change, or a truncate naming an item the phone does
  not hold, marks the pane as resyncing and sends one `OPEN`; events are
  ignored until the snapshot. Every held chat is opened again on each
  reconnect (a `RESYNC_REQUIRED` reconnects the phone). `chat_queue_full` is
  retried four times with a growing pause, then shown with Retry. Older pages
  follow the three `ChatResponse.revision` rules, and are also dropped when
  the pane's cursor changed under them (a rewind of the first message).
  `chat_cursor_invalid` brings a fresh snapshot. A pane that leaves the
  topology, or a server change, ends the chat with a `CLOSE`. A reconnect to
  another tmux server (a restart, or another host) ends a held chat instead
  of opening the same pane id there; a screen opened later on that id follows
  the new pane. An `OPEN` the host does not answer shows
  Retry; `chat_server_changed` ends the chat.
- **Routing.** `Pane.chat_adapter_id` is in the session store. The Terminal
  route redirects a chat pane to `chat/[paneId]`, so every way into a pane
  (agents, workspaces, notifications, History) lands on the chat. The Voice
  route redirects a chat pane to its chat with voice mode on.
- **Sending.** A sent message shows at once, dimmed, until the host's USER
  item with the same `client_message_id` replaces it; a refusal puts the text
  back in the composer. `chat_message_duplicate` counts as sent. When the
  answer to a `SEND` is lost (the connection drops or the host does not
  answer), the message stays and, on a live connection, a fresh snapshot is
  asked for; after a snapshot, one the timeline lacks is sent again with the
  same id. A duplicate then means it is older than the snapshot's window, so
  the placeholder goes; a refusal, or leaving the chat before it is settled,
  is a toast. While a
  request card waits, Send is held unless the provider takes messages
  mid-turn. Mid-turn, a provider that takes messages shows Send beside Stop
  once there is text; a `/btw` question can always be sent, as it does not
  touch the main turn.
- **Questions** step with Next to the last one, which has Submit, because one
  `ANSWER` carries every question's answer. `/btw` and
  `/side` (MUXFLOW commands of a provider that can fork) send `SIDE_FORK` and,
  once the topology names the fork's pane, push its chat screen; Back returns
  to the original.
  `chat_side_fork_unavailable` is said plainly until stream B wires it.
- **Edit and resend** is a long press on a rewindable message (phones have no
  hover); the banner and Cancel follow the mock.
- **Pills.** The phone mock's composer has none, so the model, effort,
  Full/Auto and Standard/Fast controls live in the Details sheet (the header's
  `i`), with the context meter. Each shows only when the capabilities allow it;
  an unlisted model leaves only the permission mode.
- **Details sheet** shows host, folder and context. Branch and Changes are not
  there: the phone has no Git messages, and stream D adds them for the desktop.
- **Permission card** is Allow / Deny. The mock's "This session" has no field in
  `ChatAnswer`, so it is left out rather than faked.
- **Voice mode** reuses `VoiceController` with two new options: `submit` sends a
  transcript as a chat message instead of typing into the terminal, and
  `registerWithHost: false` registers no `VOICE_SESSION`, so the host's hook
  replies are not pushed as well. As on the Voice screen, the controller
  outlives a reconnect, so a brief drop does not cancel a take or stop a
  reply. A Voice screen controller made for the pane
  before it was known to be a chat is ended and replaced. The mic says why
  while the chat is not ready or the agent has stopped. When a turn ends, the newest assistant
  message is read with `VOICE_SPEAK` (`speakReply`). The bar is `MicButton`'s
  new `bar` variant; the reply row is the Voice screen's `ReplyPlayer` and
  speed button. Voice mode plays replies whatever the Voice screen's autoplay
  preference says.
- **History** is a screen (`history`) opened from the Agents header. All or
  Snoozed, as the phone mock has, instead of the desktop's groups; snoozed
  rows carry Wake now on both tabs, as on the desktop. A running session opens its pane; any other resumes and
  opens where the host says it started. It lists on open and after each
  `SESSION_HISTORY` event while open. A woken session is a toast; there is no
  phone notification for it.
- **Snooze** is in the agent's long-press menu, with the desktop's default
  intervals; "Pick a time…" is desktop only. On iOS the Snooze sheet opens
  once the menu has finished closing, as iOS shows one modal at a time. It is disabled with the reason
  while the agent works or is blocked, or has no native session id yet.
- **Agent mode** is read-only in Settings ("Agents start as"), from the agent
  snapshot, which Settings asks for again when it opens.
- **Tests.** Component tests render each phone mock from scripted events with
  react-test-renderer and stubs for the native modules (`src/testing/stubs`,
  aliased in `vitest.config.ts`). With `MUXFLOW_RENDER_DIR` set they also
  write each state as an HTML page at phone width (`src/testing/renderTree.ts`).
  Nothing ran on a device.

## 6. Final manual QA

Against the devbox host, by a fresh agent that only reports observations: the
ten steps under "Manual QA" in the mock.
