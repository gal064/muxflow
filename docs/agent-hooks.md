# Codex and Claude Code hooks

Codex and Claude Code are the only V1 agent adapters. The app can launch them or
detect manually started processes.

**Hooks are the primary source of lifecycle state.** For Codex, the daemon also
watches each active child's exact turn in the transcript identified by that
child's live hook. That read-only repair path covers terminal transitions such
as cancellation that have no matching hook; it does not infer state from prompt
or tool text.
An agent whose hooks are not installed appears in the list and reads `unknown`
— the app says which agents exist and nothing about what they are doing.
Process detection proves an agent is there, and its absence retires the row
when the process exits; neither ever decides whether the agent is working,
blocked, or idle.

Hook installation is always a separate, reviewable action from remote-helper
installation. The review shows every touched file, before/after hashes, a
redacted diff, the exact managed command, and adapter trust guidance. Confirming
merges labeled entries while preserving unrelated configuration and creates a
backup. Repeating install or upgrade is idempotent.

## Whether this host reports anything

Every agent snapshot carries, per adapter, what this host's configuration
actually does with lifecycle events — observed by the daemon, not claimed by the
adapter:

| State | Meaning |
| --- | --- |
| `wired` | Every event this adapter needs invokes the managed hook, at the current owned version. |
| `partial` | Owned entries exist but do not cover every event; some transitions can never arrive. |
| `notWired` | The agent is here and nothing routes its events to this app. |
| `absent` | The agent is not on this host: no configuration, no directory, nothing on `PATH`. |
| `unavailable` | The configuration could not be inspected. Deliberately not the same as absent — it is never written over. |
| `unspecified` | The host has not answered: no snapshot yet, or a helper too old to have an opinion. Never treated as a definite answer. |

On connect the desktop asks once per host profile whether to set it up, and only
when the host reports nothing at all. The answer is remembered either way, and
`Settings → Connection → Set up agent status…` and the agents section's context
menu are where it is revisited. Consent is to keeping the host set up: when the
managed event set grows, when the host was rebuilt since it was set up, or when
an agent is installed on it later, an already-consented host is brought current
on connect without asking again — merge-only, backed up and idempotent, so a
host that is already current is not written to. Each vendor still asks its user
to trust a new hook before running it. Removing hooks records a decline, so they
are not added back until the host is set up again.

The same consent also keeps Codex hooks in their pane. Codex 0.157 and newer
run hooks inside a shared background server that has no `TMUX_PANE`, so on
every connect the helper gives the tmux server's global environment an empty
`CODEX_EXEC_SERVER_URL`, which makes Codex in new panes skip that server. It
also adds `[features] daemon_auto_start = false` to `~/.codex/config.toml`, with
a Muxflow marker comment, so Codex on the host no longer starts that server on
its own or warns about running without it. Uninstalling removes both, but only
the empty value and the marked line; anything the user set is kept. See [Troubleshooting](troubleshooting.md#codex-agents-show-unknown).

The same installer is reachable without a UI, which is how it is exercised
against a copy of a real configuration:

```
muxflow-host hook status    [--adapter codex|claude-code] [--home DIR] [--settings-path FILE]
muxflow-host hook install   [--adapter …] [--home …] [--settings-path …] [--yes]
muxflow-host hook uninstall [--adapter …] [--home …] [--settings-path …] [--yes]
```

`install` and `uninstall` refuse to change the agent configuration in your own
home directory unless you pass `--yes`. The desktop will not write a host's
configuration without a recorded answer for that host, and this command is the
same installer without an interface to ask through: `--yes` is where the answer
goes. A run redirected by `--home` or `--settings-path` changes nothing of
yours and needs no confirmation. `status` reads only, and is never gated.

On a host set up from the app, the app sets missing hooks up again on every
connect, so hooks removed with `uninstall` come back. To keep them off, remove
them from the app instead, which records that the host is no longer set up.

`--home` relocates every adapter's configuration; `--settings-path` relocates
exactly the adapter named by `--adapter`, which it requires. Without
`--adapter`, install acts on every adapter that is actually on the host, and
uninstall on every adapter that has entries of this app's to remove. Either
verb reports every adapter it did not act on and why, and exits non-zero if any
adapter failed — after printing what the others did.

## Events taken, and the gaps

Claude Code: `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PermissionRequest`, `PostToolUse`, `SubagentStop`, `Stop`, `StopFailure`,
`Notification`.

Codex: `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
`PostToolUse`, `SubagentStart`, `SubagentStop`, `Stop`, `Interrupt`,
`SessionEnd`. It has no
`StopFailure` or `Notification` (measured against Codex CLI 0.128, 0.147,
0.149.1, 0.153.4 and 0.154.0), so a turn that ends in failure is indistinguishable from
one that succeeds. `UserPromptSubmit` caches a positive auto-review result for
the exact turn before tool work begins. A `PermissionRequest` trusts that
positive cache; on a cache miss it re-reads the request's turn context rather
than caching a negative result. A permission request still unclassified after
that check and `PreToolUse(request_user_input)` are the observed blocked
signals. Recorded rather than faked.

One Codex TUI may run more than one logical thread while all of their hook
processes inherit the same `TMUX_PANE`. Pane ownership therefore does not
follow whichever native session emitted the latest hook. The pane retains one
foreground session and a bounded chain of the sessions it displaced. An
unknown native Codex session may enter the foreground only with `SessionStart`
or `UserPromptSubmit`, the two events that explicitly begin a session or turn.
Its `Stop` remains foreground because that session can accept another prompt.
`SessionEnd` restores its immediate predecessor; the predecessor's first root
hook may also restore it once every newer session is terminal. Returning to an
older predecessor permanently unwinds the newer sessions, so their delayed
tool, child, or teardown hooks cannot reclaim the pane. Suspended sessions keep
their own lifecycle and turn state and never seed the foreground session's
state. The chain is limited to sixteen predecessors.

Pane ownership is stored separately from the verified navigation route. When
fresh topology is temporarily unavailable, that binding can reject a different
logical session, transfer ownership for an explicit replacement, or restore a
recorded same-pane predecessor without inventing a routing destination. A hook
matching the current native owner keeps that owner's last verified route
through the outage. A verified move keeps the moving session's state and
retires the other pane owner.
The initial pane-derived record may still be promoted by any first native hook,
including `Stop`; that is the separate discovery handoff needed when hooks
become authoritative late in a turn, and it preserves the last verified route
even if topology discovery fails during that hook.

Every native session has one canonical identity whether its pane is currently
visible in topology or not. A missing-pane hook therefore updates that one
session or is rejected by its pane binding; it cannot create an alias that later
needs ranking, merging, tombstoning, or voice-session migration. Only the
initial pane-derived manual identity is promoted when the first native hook
arrives.

Session and turn handling form one authority hierarchy: pane, native session,
root turn, then child turn. Session authority is resolved before deduplication,
transcript repair, lifecycle, attention, identity promotion, or voice reply
delivery; only an accepted native session can affect its root and child turns.
A superseded-session discard writes one safe daemon diagnostic containing the
pane ordinal, hashed agent IDs, and normalized hook event name, never native
session or turn IDs or hook content.

A `Working` state that receives no further event for fifteen minutes decays to
`Unknown`. Direct evidence of a running subagent stays authoritative while its
bounded transcript monitor remains readable. The 24-hour recovery window is a
final backstop only when neither hooks nor a readable transcript remain, so
long child tasks with a readable transcript stay Working until their exact
terminal edge. Attention already earned survives; only the claim about right
now is dropped when the backstop applies. The ordinary bound is the longest gap a healthy agent can
otherwise leave — one tool call, at Claude's maximum configurable `Bash`
timeout of 600s — with half again for headroom.

Managed Codex hooks use its three-second maximum for `Interrupt` and
`SessionEnd`; the other managed hooks use five seconds. Codex already clamps
the terminal lifecycle hooks to three seconds, so matching that limit avoids a
startup warning without changing their effective runtime allowance.

Use the same review flow with Uninstall to remove only entries labeled
`muxflow-managed`. Do this before uninstalling the desktop package. The
package uninstaller refuses to remove the helper while a known managed marker
is present, preventing silently broken agent configuration.

Hooks submit compact state JSON through the private daemon socket and never send
prompt text, terminal output, tool input, file contents, or credentials. The
only transient locator is Codex's transcript path on a live hook. The daemon
opens it through the confined transcript reader and retains only the validated
open file handle in memory. For a confirmed user-reviewed request it checks the
file length during the existing maintenance pass and clears Blocked when that
exact turn appends `turn_aborted` or `task_complete`. Each child-scoped hook
identifies one opaque child ID and exact turn ID. Any activity on a newer child
turn reopens that child; `SubagentStop` or a matching transcript terminal clears
only that turn. This covers Codex cancellation, which can emit no clearing hook,
without treating the parent-directed `SubAgentActivity(interacted)` record in a
child transcript as another child. Monitors and retained child IDs are capped
at 64, every opaque child ID is capped at 1 KiB, and each read
is limited to the final 1 MiB after file growth. If no daemon accepts the event, the hook materializes only those
sanitized terminal transitions found in the root transcript, resolves the
reviewer when needed, and strips the path before placing the event in the
durable fallback mailbox. The
hook process accepts a bounded vendor envelope up to 64 MiB because tool-complete
events can include the full result, including base64 image data. It parses that
envelope as a stream and retains only the lifecycle allowlist, so large tool
results do not become large daemon messages or durable mailbox entries.
Retained lifecycle strings are individually limited to 16 KiB and the compact
daemon payload remains limited to 256 KiB. The
normalized Codex `PreToolUse` payload retains only the tool name needed to
distinguish a question from ordinary work. Normalized Codex lifecycle events
retain only the opaque turn ID, and turn-start/permission events retain the
reviewer needed for exact-turn revalidation. The daemon keeps a bounded set of
parent and child reviewer decisions so concurrent children cannot evict one
another. An unresolved permission reviewer stays Working; only an exact `user`
decision raises Blocked. Codex child-scoped events retain only their opaque
agent and turn IDs; the daemon keeps a bounded child-to-turn map so duplicate
events cannot miscount concurrent children, a resumed turn replaces its prior
turn, and a late terminal event cannot clear the resumed child. Hooks update
that map immediately and exact-turn transcript terminals repair missing stops.
A Codex goal can complete one root turn and automatically start another without
emitting `UserPromptSubmit`. The daemon therefore also tracks exact root turn
IDs and their UUIDv7 ordering: activity from a fresh root turn reopens Working,
while later tool or terminal hooks from an older or recently completed root
cannot overwrite the newer turn. The exact completion set is bounded because
UUID ordering still rejects older entries after eviction. The daemon also retains
a bounded recent history of each child’s latest exact turn. Because child hooks
do not name their parent root, first-seen child ownership remains explicitly
unknown; a known child that resumes after its root completes waits for the next
root hook before binding. Transcript fallback can then finish an unknown or
matching older child without clearing the same child ID after a known resume
under a newer root.
A parent `Stop` remains Working until that map is empty,
including across daemon restarts, while a real Blocked state remains Blocked
until its own question or permission resolves. A Claude `Stop` retains only a boolean
saying whether a subagent is still running; task descriptions, commands, IDs,
and the rest of Claude's background-task payload are discarded. The daemon
retains that boolean until the final `Stop`, including across restarts, so
Claude's routine idle notification cannot misreport a long-running subagent as
blocked. Malformed or stale events are rejected. A real permission request
remains blocked even if an idle notification follows it. The 24-hour recovery
bound uses a separate child-evidence clock, so unrelated hooks cannot keep stale
evidence alive. A temporarily unavailable daemon retains only bounded, atomic
hook state.

One field carries content, for voice mode only: a `Stop` from either adapter
forwards `last_assistant_message` — the agent's final message of the turn —
cut at 32 KiB on a character boundary with `last_assistant_message_truncated`
set when it was cut. The daemon reads it once on ingest, hands it to the voice
service to speak to a phone that registered a voice session for that agent,
and stores none of it: `AgentRecord`, `agents.json` and every agent event are
shaped by the lifecycle alone. The one place it can rest on disk is the same
place every other hook payload can: the private (0600) fallback mailbox in the
durable per-user state directory, written only when no daemon answered and
deleted when the daemon drains it. No other event forwards it, `StopFailure`
included. A host that never opened voice mode receives the field and drops it.

For validation, use installed `codex --version` / `codex --help` and
`claude --version` / `claude --help` only. Release QA must not send prompts or
perform network-backed agent work with the user's accounts.
