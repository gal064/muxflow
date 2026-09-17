# Phase 13 — Agent status that tells the truth

Field evidence (2026-08-14, user hands-on against remote-linux) established that agent
status is broken at the first mile, and the user has asked for a full phase:
"notifications and agent status need a full thing... it needs probably a full
phase to fully test."

## Root cause, verified

1. **No hook events ever reach the daemon on the field machine.** The user's
   `~/.claude/settings.json` on remote-linux wires every hook to an existing tool's script
   (`/home/operator/.existing-agent-hooks/agent-hooks/claude-hook.sh`); this app's `hook ingest` was
   never installed there. The daemon's store (`/tmp/tmux-agent-ide-1000/agents.json`)
   showed every record with `detected_manually: true`, `source_event_ids: []`,
   `latest_source_generation: 0`.
2. **Manual pane-scan detections are rendered dishonestly.** Reconcile creates
   them with `AgentLifecycleState::Unknown` (`apps/host/src/service/agents/reconcile.rs:95`
   area), and the desktop renders that as an active/working treatment — the user
   saw "inductive" and "les" as working while both were fully idle, with no
   transition ever (nothing feeds one).
3. **No status reaches the window tabs** (no activity dots), and workspace-row
   aggregation was never exercised with real hook-sourced states.

## User decisions (2026-08-14, recorded)

- **Hook installation: auto-install on connect** with a one-time consent prompt
  per host, merging alongside existing hooks (an existing tool's must keep working) — never
  replacing, never silent.
- **Notifications: in-app catch-up only.** No native macOS notifications, no
  menubar helper. The daemon records attention while the app is away; on
  reconnect the sidebar/tabs must show everything that happened ("hey, go check
  out this agent"), with done-unread staying lit until the pane is focused.

## 13.1 Hook installation and health

1. Host-side installer: `tmux-ide-host hook install` — idempotent, merge-only.
   Adds `hook ingest` invocations to the Claude settings hook arrays for the
   lifecycle-relevant events (SessionStart, UserPromptSubmit, PreToolUse,
   PostToolUse, Stop, StopFailure, SubagentStop, Notification/permission_prompt),
   alongside whatever is already there; writes a timestamped backup before any
   edit; `hook uninstall` removes exactly what it added (identified by a
   marker), restoring nothing else; `hook status` reports per-adapter wiring
   (claude wired / codex wired / not wired). Codex: wire its notify/hook
   mechanism equivalently; if codex has no usable hook surface for an event,
   document the gap rather than faking it.
2. Installer must take `--settings-path` / `--home` overrides so QA can run
   against isolated fixtures. **QA never edits the real `~/.claude` or
   `~/.existing-agent-hooks` on remote-linux.** The one real install on the user's machine happens
   through the consent flow with the user present.
3. Desktop: on connect, query `hook status`; if not wired, show the consent
   prompt once per host profile (choice persisted, re-offerable from Settings).
   On consent, run the installer over the existing connection and re-check.
4. Honest empty state: while hooks are not wired, the agents section shows one
   quiet line ("agent status unavailable on this host — set up hooks") instead
   of stale guesses.

## 13.2 Honest lifecycle semantics

1. `Unknown` (manual detection) renders as a neutral "detected" treatment —
   hollow dot, no spinner, never the working/blocked/done colors. Working,
   blocked, and done-unread come only from hook-sourced events.
2. Staleness: a hook-sourced Working with no further events past a TTL degrades
   to Unknown (not stuck-working). Tune the TTL against real claude cadence
   (PreToolUse/PostToolUse arrive per tool call; long single tool calls must
   not flap) and record the chosen value and why.
3. Verify end-to-end transitions: Stop → done-unread (attention), permission
   prompt → blocked (attention), pane focus → seen (`seen_generation`), next
   prompt → working again.

## 13.3 Attention and catch-up (in-app only)

1. Disconnect catch-up: with the app disconnected, drive hook events through
   `hook ingest` on the host; on reconnect the sidebar must light done-unread /
   blocked rows and unread badges purely from the daemon's persisted state.
   Automated test required, not just QA.
2. Priority ordering (blocked > done-unread > working > idle) and ⌘⇧U verified
   with real hook-sourced states, not synthetic store fixtures only.
3. No native notification code in this phase (user decision above).

## 13.4 Status on every surface, from one selector

1. One derivation feeds all three surfaces — sidebar agents list, workspace
   rows (loudest-agent aggregation), and window-tab activity dots/spinners.
   Terminal tabs show the loudest state of agents in that tmux window;
   document tabs unaffected.
2. Unit tests per surface mapping; the accessibility glyph option applies to
   every dot site (a Phase 11 round-5 fix regressed to one of three — guard it).

## 13.4b Fresh-machine independence (from the 2026-08-14 worktree-cli audit)

Both dev machines run worktree-cli's `~/.tmux.conf`; the IDE was never
validated against stock tmux. Verified: core paths (control mode, pause,
capture, `send-keys -H` input, ID-based routing) read no user config, and
`window-size latest` is the stock default. Remaining items, owned here because
this phase touches routing and builds isolated fixtures anyway:

1. Stock-config QA lane: run the agent-status end-to-end scenario (13.5) and a
   UI smoke on a `tmux -f /dev/null` server — stock defaults: `base-index 0`,
   `allow-rename off`, `mouse off`, `history-limit 2000`. Gate: everything
   functional passes; record cosmetic differences honestly.
2. Defensive fix: `window_name_fallback` re-routing (identity.rs) must not
   misroute when several windows share a generic name (`claude`) — prefer
   ID/route evidence, treat an ambiguous name match as no-match.
3. Window naming is owned by §13.5 (superseding an earlier app-side-display
   draft): the user's decision is that names must be right in tmux itself.
   The name-based fallback routing in identity.rs may additionally match
   `pane_title` (streamed over the control channel) as corroborating
   evidence, which helps on hosts where §13.5 hasn't been applied.

## 13.5 Recommended tmux config, applied by the daemon (non-gating)

Low priority per the user ("not a big deal right now") — do this last within
the phase, and it may slip to the backlog without blocking Phase 13 exit.

User decision (2026-08-14): the app's contract is "tabs are tmux windows" —
window names must be right in tmux itself, not prettified app-side. Today the
descriptive names come from worktree-cli's user config
(`set-hook -g pane-title-changed` syncing agent pane titles to window names,
plus a `pane_current_command`-based `automatic-rename-format`); a fresh
machine shows generic "claude"/"bash" names, which also weakens the daemon's
`window_name_fallback` routing (three windows all named "claude").

1. Define the minimal recommended set the product depends on: the
   pane-title→window-name sync hook and the automatic-rename format. Nothing
   cosmetic (status bar, bindings stay the user's business).
2. The daemon applies it with tmux commands at attach — in-memory only, never
   writing `~/.tmux.conf` — idempotent, re-asserted on reconnect and after
   tmux server restarts. Consent-gated per host exactly like hook install
   (one prompt covers both: "set up this host").
3. Detection: if the user's own config already sets an equivalent hook/format
   (as on the user's machines), do not override or duplicate it.
4. QA on isolated tmux fixtures: fresh stock server → names follow agent
   titles in both the app and a plain client; hook applied twice → one hook;
   user-config-present fixture → untouched.

## 13.6 End-to-end verification on remote-linux

1. Scratch session (`ade-phase13-*`), isolated daemon + isolated HOME fixture:
   run a real claude-code (or scripted `hook ingest` sequence reproducing its
   exact event order) through working → blocked → working → done-unread → seen,
   asserting the app UI at each step (machine-read, cua-driver AX/screenshots).
2. Measure hook-event → UI latency; gate < 1s on the private overlay network link.
3. Disconnect/catch-up scenario from 13.3 exercised against the real link.
4. Installer idempotence and merge-preservation tested against a fixture copied
   from the real shape of the user's settings (existing terminal tool hooks present); running
   install twice changes nothing; uninstall leaves an existing tool's entries untouched.

## Acceptance

- With hooks wired: no stale working states; idle agents show idle; Unknown
  never renders as working; transitions land in the UI < 1s after the hook.
- With hooks absent: the honest empty state, no guesses.
- Catch-up after disconnect works and is covered by an automated test.
- Installer: merge-only, idempotent, backed-up, uninstallable; existing terminal tool unaffected.
- Existing gates hold: resting controls ≤ 8, one connection indicator,
  `run-app-client-size.sh` green, perf budgets unregressed.

## Non-goals

Native/menubar notifications (explicitly declined for now); new agent adapters
beyond claude-code/codex; any change to terminal data paths.

Also deferred (user decision 2026-08-14): the dual-client window-size policy.
When the app and a plain terminal share a session, tmux's `window-size latest`
means the last-active client sizes the window — the other letterboxes or, in
the app's case, clips. The user keeps interim behavior as-is (they intend to
stop attaching plain terminals once the app is solid) and the policy gets
designed when mobile lands, since mobile forces per-client sizing anyway.
