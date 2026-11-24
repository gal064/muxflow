---
name: muxflow
description: Use when starting, reading, or messaging Claude Code, Codex, pi, OpenCode or other CLI agents running in tmux on a Muxflow host, or when asked to create, edit, pause, or check a Muxflow automation (a scheduled agent run).
---

# Muxflow

Muxflow runs agents as tmux panes. Its helper tracks the agents it has adapters
for (Claude Code, Codex, pi) in `agents.json` in the state directory:

- Linux: `~/.local/state/muxflow/`
- macOS: `~/Library/Application Support/dev.muxflow.desktop/`

## Find an agent

Query `agents.json` for the record you need, not the whole file. Each entry in
`.agents` has `adapter_id` (`claude-code`, `codex`, `pi`), `native_session_id`,
`lifecycle` (1 working, 2 blocked on the user, 3 idle, 4 unknown: no hooks) and `route.pane_id`.

```sh
jq '.agents[] | {adapter_id, native_session_id, lifecycle, pane: .route.pane_id, session: .route.session_name_fallback, window: .route.window_name_fallback}' ~/.local/state/muxflow/agents.json
```

Then check the pane's mode. `tmux show-options -pqv -t <pane> @muxflow-chat` is
empty for a terminal pane and names the adapter for a chat pane. pi always runs
in a terminal pane.

An agent missing from `agents.json` (OpenCode until its adapter lands, or any
other CLI) is an ordinary terminal pane: find it with
`tmux list-panes -a -F '#{pane_id} #{pane_current_command} #{pane_title}'`, and
read and message it as a terminal pane.

## Read an agent

Its transcript is the reliable source in both modes.

- Claude Code: `~/.claude/projects/*/<session-id>.jsonl`
- Codex: `find ~/.codex/sessions -name "*<session-id>.jsonl"`
- pi: `ls ~/.pi/agent/sessions/*/*_<session-id>.jsonl`

Read only the latest user and assistant turns. In a terminal pane,
`tmux capture-pane -p -t <pane> -S -60` shows live state. An idle prompt does not
mean the task is done.

## Start an agent

Pass the task on the command line. In chat mode it is sent as the first message,
and the terminal session it replaces may have begun on it too, so keep a
task that has side effects safe to start twice.

```sh
tmux new-window -d -P -F '#{pane_id}' -n <name> -c <dir> "claude 'TASK'"   # also: codex 'TASK', pi 'TASK', opencode --prompt 'TASK'
```

In chat mode, `@muxflow-chat-session` on the pane holds the session id once the
first turn starts.

## Message an agent

**Terminal pane** (pi, OpenCode, and any agent in Terminal mode): `tmux send-keys -t <pane> -l 'MESSAGE'`, then
`tmux send-keys -t <pane> Enter`. Wait 5 seconds, capture the pane, and confirm
it was submitted.

**Chat pane:** the pane shows "Muxflow chat session". Anything typed into it is
discarded. Send through the helper, which works for Claude Code and Codex:

```sh
muxflow-host chat send <pane> 'MESSAGE'    # or pass - and pipe the message in
```

On a Mac the helper is `/Applications/Muxflow.app/Contents/MacOS/muxflow-host`.
It returns once the message is accepted. Read the reply from the transcript.
Never connect to the pane's `@muxflow-chat-socket` yourself: a new connection
takes the agent over from Muxflow.

## Automations

An automation is a cron schedule that opens a new tab in the `automation` tmux
session and runs an agent with a prompt. Create one by editing
`automations.json` in the state directory. There is no CLI. The helper picks up
changes within 15 seconds. Keep any entries already in the file.

```json
{
  "version": 1,
  "automations": [
    {
      "id": "review-prs",
      "name": "Review open PRs",
      "schedule": "0 9 * * 1-5",
      "cwd": "~/dev/project",
      "agent": "claude",
      "prompt": "Review open PRs and comment on real problems. If there are none, say so and stop."
    }
  ]
}
```

- `id` must match `[a-z0-9-]{1,64}` and be unique. `agent` is `claude`, `codex`,
  or `custom` (custom also needs `command`; the prompt is added as its last
  argument). For pi, use `"agent": "custom", "command": "pi"`.
- Optional fields: `enabled` (default true), `keepTabs` (1–50, default 5),
  `maxRuntimeMinutes` (5–1440, default 120).
- Cron has five fields in the host's local time. There are no `@hourly` macros.
- Runs are unattended: Claude in auto mode, Codex with `--approve-for-me`. Write
  the prompt so that a run with nothing to do stops.
- Run history and errors in the file are in `automations/runs.json`. Check it
  after you edit.
- The agent must already trust `cwd`; otherwise the run waits on the trust
  prompt. Missed runs are skipped, not caught up.
