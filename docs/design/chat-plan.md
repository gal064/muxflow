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
message ChatResponse { ChatSnapshot snapshot; repeated ChatItem older; string older_cursor;
                       string forked_pane_id; ChatAgentMode mode; }

message ChatItemDelta { string item_id; string text_append; }
message ChatTruncate { string after_item_id; }   // rewind: drop everything after this item
message ChatEvent { string pane_id; uint64 revision;
                    oneof change { ChatItem item; ChatItemDelta delta; ChatState state;
                                   ChatCatalog catalog; ChatTruncate truncate; } }
```

Semantics:

- `OPEN` subscribes the connection to one pane and returns a snapshot. Chat
  events go only to subscribed connections (`send_control_event_to`).
- `revision` increases by one per event per pane. A client that sees a gap
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

## 6. Final manual QA

Against the devbox host, by a fresh agent that only reports observations: the
ten steps under "Manual QA" in the mock.
