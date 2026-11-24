# Codex chat parity — root causes and fix plan

Codex chats lag Claude Code chats: no `/compact`, no subagent view, warnings
that never show, and items Codex now sends that are dropped. One finding
applies to both providers: the background chip sits in the middle of the
composer instead of on its left.

Investigated against the source and the app-server schema of Codex 0.171.0
(`codex app-server generate-json-schema`). The Codex adapter was written
against 0.169.0. Nothing here was run live. Each item marks what still needs a
live check.

Mocks: [chat-task-chip-mock.html](chat-task-chip-mock.html) (item 1),
[agents-header-history-mock.html](agents-header-history-mock.html) (item 7),
[pane-rings-mock.html](pane-rings-mock.html) (item 8).

## 0. Summary

| # | Item | Size | Where | Wire |
| --- | --- | --- | --- | --- |
| 1 | Background chip left-aligned (both providers) | XS | desktop | none |
| 2 | Codex built-in commands: `/compact`, `/review`, `/init` | S | host | none |
| 3 | Codex warnings, retries, model reroute, new item types | S | host | none |
| 4 | Codex subagents on a par with Claude's | M | host | none |
| 5 | Codex plan (task list) | S–M | host, maybe desktop | maybe additive |
| 6 | Subagent drill-in (both providers) | L | all | additive |
| 7 | Sidebar headers: both toggles grey, no chevron | XS | desktop | none |
| 8 | Pane frames: attention frame removed, active pane grey | S | desktop | none |

Items 1–4 need no wire change, so no `PROTOCOL_MAJOR` question arises. The
phone picks up 2–4 for free, because the catalog and items come from the host.

## 1. Background chip left-aligned (XS)

**Root cause.** `composer.rs` `background_chip` puts the popover in an
absolute `div` with `left(10)` and `right(10)`. The kit's popup trigger
wrapper is a block element, so it stretches to that full width, and the kit's
`Button` sets `justify_center` on its content. The pills end up centred. Both
design mocks (`chat-polish-mock`, `subagents-visible-mock`) draw `.edge` as
`display:flex`, which puts the pills on the left, so the code does not match
the design.

**Fix.** Make the absolute wrapper an `h_flex()` so the trigger shrinks to
its content and starts at `left(10)`. Keep `trigger_style(max_w_full)` so a
long agent pill still truncates. The list is anchored `BottomLeft`, so it
opens over the chip.

**Tests.** A layout test: the chip's left edge equals the composer's left
edge + 10. It must also still truncate at a narrow width.

## 2. Codex built-in commands (S)

**Root cause.** Claude's catalog is Claude Code's own command list, and
Claude Code runs a `/compact` it is sent. Codex's catalog
(`codex.rs` `publish_catalog`) is Muxflow's `/btw` and `/side` plus skills.
A typed `/compact` goes to the model as an ordinary message.

**Design.** The host lists the commands as `CHAT_COMMAND_KIND_COMMAND` and
intercepts them on `SEND` in the Codex dialect:

| Command | Request | Notes |
| --- | --- | --- |
| `/compact` | `thread/compact/start { threadId }` | The compacting line and the "Context compacted" notice exist already (`contextCompaction` item) |
| `/review [branch\|sha]` | `review/start { threadId, target }` | No argument: `uncommittedChanges`. A branch name: `baseBranch`. A hex sha: `commit`. Anything else: `custom { instructions }`. Needs `enteredReviewMode` / `exitedReviewMode` drawn (item 3) |
| `/init` | `turn/start` with the fixed AGENTS.md prompt | Same as the Codex TUI; an ordinary turn |

- The typed command is shown as the user's message, as on Claude. It cannot
  be rewound to (it opens no turn of its own).
- While a turn runs, the command waits in the `held` queue and is sent when
  the turn ends, as a message would be. `held` gets a variant for it.
- A refused request gives a notice, as `Asked::Turn` failures do.

**Left out, on purpose.**
- `!cmd` (`thread/shellCommand`) and `/new`: not offered (owner, 23 Nov).
  Open a new terminal instead. `!cmd` would also run unsandboxed whatever the
  chat's permission is.
- `/mcp`, `/status`, `/diff`. MCP status needs a surface. Status and diff are
  already in the details card and the Git view.

**Live check.** One `/compact` on a short thread, to confirm the server sends
the `contextCompaction` item and that it does so outside a turn. One
`/review`, to see what the review-mode items carry.

**Tests.** Catalog lists the three. `/compact` sends `thread/compact/start`
and no `turn/start`. A `/compact` while busy waits for `turn/completed`.
`/review main` → `baseBranch`. `/review` with no argument →
`uncommittedChanges`.

## 3. Warnings, retries, reroute, new item types (S)

**Root cause.** `Codex::notification` ends in `_ => {}` and `thread_item` in
`_ => return None`. Since 0.169 Codex sends more than they know.

| Codex sends | Now | Plan |
| --- | --- | --- |
| `error` with `willRetry: true` | dropped; a retrying turn looks hung | A notice "Retrying: reason", one per turn, rewritten in place under the same id. A status-line "Reconnecting…" would need a new state field, so it is not done here. Live check: does Codex send anything when a retry succeeds? |
| `warning`, `configWarning`, `deprecationNotice` | dropped | A notice row each, de-duplicated by text per session |
| `model/rerouted { fromModel, toModel, reason }` | dropped; the model changes silently | A notice: "Switched from X to Y: reason". Update `options.model` |
| `enteredReviewMode` / `exitedReviewMode { review }` | dropped | Entered: notice "Reviewing …". Exited: the review text as an assistant message |
| `imageView { path }` | dropped | A tool row "view" with the path |
| `imageGeneration` | dropped | A tool row with `savedPath`; an attachment if the desktop can show it. Live check |
| `sleep { durationMs }` | dropped | A tool row "wait 30s" |
| `hookPrompt`, `functionCallOutput` | dropped | Keep dropped; write that down in the match so it is a choice |
| `agentMessage.phase = commentary` | drawn like the answer | No change. Owner: be consistent with Claude Code. Both providers already map mid-turn text to an ASSISTANT item, and `timeline::layout` does not depend on the provider, so the two are drawn alike |

**Drift guard: dropped.** A checked-in list of names only catches drift when
someone regenerates it, and it would add a classification list to the adapter.
Not worth the code.

**As built (live, 0.171.0).** Every thread warned "Under-development features
enabled: default_mode_request_user_input", a feature the host turns on itself.
The pane command now also passes `-c suppress_unstable_features_warning=true`,
so the warnings that are shown are the user's. A review runs as an inner turn
whose `turn/started` names its outer turn in `rootTurnId`; only the outer one
completes. The chat now matches a completion against the running turn or its
root, otherwise a review would leave it working for good.

## 4. Codex subagents on a par with Claude's (M)

**Root cause.**
- Each subagent is a thread of its own. `Codex::notification` drops every
  notification whose `threadId` is not the chat's thread, so the subagent's
  own items, token usage and turn end are never read.
- The row is built only from the parent's `collabAgentToolCall` items. It has
  no `agent_type`, `activity`, `tool_uses` or `tokens`. Its state changes only
  when the parent calls `wait` or a similar collab tool.
- `turn_completed` stops every subagent row when the parent's turn ends,
  whether or not the subagent is still running.
- Subagents never enter `state.background`, so a Codex subagent never gets
  the accent pill on the composer that Claude's get.

**Design.** Everything goes onto existing wire fields.

- `subagents: BTreeMap<thread_id, item_id>`, filled from the spawn's
  `receiverThreadIds[0]` (as `remember_agent` does today).
- Notifications for a known subagent thread are routed to `subagent_event`
  instead of being dropped:
  - `item/started` → `activity` ("Running `cargo test`", "Editing a.rs",
    "Searching …"), using the existing row text for that kind.
  - `item/completed` of a tool kind → `tool_uses += 1`.
  - `thread/tokenUsage/updated` → `tokens` = total.
  - `turn/completed` → row done or failed, and removed from `background`.
- A running subagent goes into `state.background` as a `SUBAGENT` task
  (description = the row's title, `stoppable: false`). The desktop's
  existing accent pill and list then work unchanged.
- `turn_completed` no longer stops rows of subagents that are still running.
  They end by their own `turn/completed`, a collab call that says they have
  ended, or when the agent process ends.
- Republishing goes through the existing 50 ms window, so a busy subagent
  does not flood the link.
- Stop: `collab interruptAgent` is a tool of the model's, not a client
  request. No stop button for now, the same as Codex background commands.

**Live check (done, 0.171.0).** The server sends a spawned subagent's
`turn/started`, `item/*`, `thread/tokenUsage/updated` and `turn/completed` on
the same connection, under the subagent's thread. It sends no `thread/started`
for it and no `subAgentActivity` item, so there is no agent type to show: the
row reads as the prompt it was given.

**Tests.** Composed from the recorded shapes (the recording holds local
paths and is not checked in). The row gets activity and rising figures,
stays running across the parent's `turn/completed`, and the background list
fills and empties.

## 5. Codex plan (S–M)

`turn/plan/updated { explanation, plan: [{ step, status }] }` is dropped.
Claude's TodoWrite is only a row that says "Update the task list", so the two
providers are equal today, and both are weak.

- **Cheap:** a tool row "plan" whose output preview is the checklist, updated
  in place. Host only.
- **Better, for both providers:** a pinned checklist above the composer.
  This needs a `ChatState.plan` field (additive) and a mock. Leave it until
  1–4 are in.

## 6. Subagent drill-in (L, both providers)

Clicking a subagent opens its own conversation read-only. Deferred in the
polish plan (§3.14, "Not in this cut") for Claude, and not built for Codex.
For Codex the host already has the events once item 4 routes them. For
Claude it means keeping the `parent_tool_use_id` lines that are dropped today.
Needs its own plan, mock, and a wire design (a sub-timeline per item, or a
second chat id). Do not start before item 4 is in.

## 7. Sidebar Agents header (XS)

Asked for on 23 Nov. In `ui/chrome/sidebar.rs`:

- The history clock (`agents-history`) stays on the right, after the
  ordering toggle, matching the Workspaces header's toggle and + (owner,
  23 Nov).
- The ordering toggle drops its chevron (`sort_toggle_with_chevron` becomes
  `sort_toggle`) and turns `chrome_dim` instead of `accent`, then
  `chrome_ink` on hover.
- The Workspaces header's "all / pinned only" toggle turns `chrome_dim`
  too (owner, 23 Nov), so both headers read the same.

## 8. Pane frames (S)

Asked for on 23 Nov. `terminal/surface.rs` draws two accent frames:

- **Active pane of a split:** a 2px accent square just outside the pane.
- **Agent wants you:** a rounded 2.5px accent frame inside a pane whose
  agent is Done or Blocked and has not been typed or clicked in since.

**Attention frame: removed, with the code that exists only for it.**
- `PaneAttention` (`terminal/integration/agents.rs`).
- Its observer in `surface.rs`, and the `wants_attention` overlay.
- `AgentsRuntime.attention`, the ring sets built in `changed()`, and
  `attention_rings()` (`agents/runtime.rs`).
- Kept: `note_activity` / `PaneActivity`. Typing or clicking still marks the
  agent seen, which clears the Agents list dot and the notification state.
- Tests: `rings_a_waiting_pane_and_typing_in_it_acknowledges_the_exact_generation`
  and `a_refused_seen_mark_keeps_the_ring_and_says_why` keep their
  acknowledgement assertions and drop the ring ones; the `rings` and `wants`
  helpers go.

**Active pane: grey, pending the owner's look at the mock.** A 1px border,
one step darker than `chrome_border` (light `#c4c4cc`, dark `#4b5263`), on
the active pane of a split only, for terminal and chat panes alike. The ring
geometry stays as it is. Only the width and colour change. If the owner
prefers no border, the `rings` block and `FOCUS_RING` go as well.

## 8a. Wire review

Items 1–4, 7 and 8: no wire change. Item 5 (better) and item 6 add fields.

- Old phone, new host: the phone ignores the new fields and keeps today's
  rendering.
- New phone, old host: the fields are absent and the phone renders as today.

So no major bump, but answer this again in the PR when the fields exist.

## 9. Order and budget

1, 7 and 8 → 2 → 3 → 4 (live check first) → 5 cheap. 6 is separate.

Live Codex turns, cheapest model, one-line prompts: about 4 in total (one
`/compact`, one `/review`, one subagent spawn, one to confirm). They run on a
private daemon and tmux socket, never the owner's live daemon or `~/.codex`.

## 10. Owner's answers (23 Nov)

1. Subagent view: parity with Claude now (item 4), drill-in later (item 6).
2. No `!cmd` and no `/new`: open a new terminal.
3. Codex commentary: consistent with Claude Code. Already the case; no
   change.
4. No `experimentalApi`. Codex background commands stay unstoppable and are
   not shown after a reopen.
5. Both sidebar toggles grey, no chevron; history clock stays on the right (item 7).
6. Pane frames: no ring at all, neither the active-pane ring nor the
   attention frame (item 8).

## 11. Wide tables and images in the Markdown preview (not done)

Asked for: tables and images use the pane's full width, and only text keeps
the 720px column. The preview's column is padding on Kit's scrolling
`TextView`, and that view clips its content at the padding. A table given
`w_auto` and a negative margin does widen (checked headlessly), but the
scrolling view clips it at the column edge. The other way round, a
full-width view with text narrowed to 720px, needs a paragraph and image
style hook that Kit 0.10.0 lacks. Either way it is a Kit change, so it gets
its own PR and Kit bump.
