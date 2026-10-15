# Automations: scheduled agent runs owned by the host helper

Status: implemented. Phase 1 is the helper; phase 2 is the desktop panel and
its wire, see [Desktop panel](#desktop-panel-phase-2).

Mock: [design/automations-mock.html](design/automations-mock.html) (open locally).

## Summary

An automation is a cron schedule, a working directory, a command and a prompt,
stored as JSON on the host. The helper daemon runs it. Each run opens a new tab
in one dedicated `automation` workspace (tmux session) on that host and types
the command with the prompt appended. Claude Code and Codex have built-in,
fully unattended default commands, and any other command can be used instead.
The helper tracks each run's status through the existing agent hooks. It skips
a run while the previous one is still working and keeps only the newest N tabs
per automation.

The feature is built to be easy to remove: its code lives in one host folder
and one desktop folder behind two small, general extension points, plus one
separate proto file. See [Removal](#removal).

## Using automations

Write `automations.json` in the helper's state directory, or ask an agent to:

- Linux: `~/.local/state/muxflow/automations.json`
- macOS: `~/Library/Application Support/dev.muxflow.desktop/automations.json`

```json
{
  "version": 1,
  "automations": [
    {
      "id": "fix-ci",
      "name": "Fix failing CI",
      "schedule": "0 9 * * 1-5",
      "cwd": "~/dev/muxflow",
      "agent": "codex",
      "prompt": "Check the latest CI run on main. If it failed, fix it on a new branch and open a PR. If CI is green, say so and stop."
    }
  ]
}
```

Or turn on "Show Automations" in the desktop's Settings, Workspace tab, and use
the Automations panel beside Files, Git and Ports.

The helper notices changes within 15 seconds. Each run opens a tab named
`Fix failing CI #<n>` in the `automation` workspace, which shows up on desktop
and mobile like any other workspace. Run history, and anything wrong with the
file, is in `automations/runs.json` next to it.

An agent can set one up from a sentence such as: "Add an automation to
`~/.local/state/muxflow/automations.json` that runs Claude in ~/dev/muxflow
every weekday at 9:00 and reviews open PRs. Read docs/automations.md for the
format."

Things to know:

- **Unattended.** The default commands skip every permission prompt, and Codex
  also runs without its sandbox. They act with your full user permissions.
- **Write the prompt for an empty run.** There is no precheck; tell the agent
  to stop when there is nothing to do.
- **Pausing.** Set `"enabled": false`.
- **The helper must be running.** Closing the desktop does not stop
  automations. After the host reboots they don't run until a desktop connects
  and starts the helper again. Minutes missed while the helper was down or the
  host was asleep are skipped, not caught up.
- **Agent hooks.** A Claude Code or Codex run only shows as `done` when
  [agent hooks](agent-hooks.md) are set up. Without them it reads as `working`
  until `maxRuntimeMinutes` passes, and only then can the next run start and
  its tab close.
- **Old tabs close.** The newest `keepTabs` tabs of each automation stay open.
  Older ones close once their run has finished or passed `maxRuntimeMinutes`,
  unless an agent in the tab is working or waiting on you. Tabs of an
  automation you delete stay until you close them.
- **First run in a new directory.** On the machine tested, neither default
  command stopped at a folder-trust or bypass confirmation. If an agent version
  does, the run waits in its tab, reading as `working` (or `needs_attention`
  if a hook reports the question), until you answer it once.

## Decisions

Settled with the user:

- The helper owns the automations, stores them as JSON, and runs them whether
  or not a desktop is connected.
- There is one `automation` workspace per host, and each run is a new tab in it.
- Each automation has its own working directory.
- Schedules are cron only, evaluated in the host's local time. There are no
  presets and no time zone field.
- There is no catch-up for missed runs. If the helper is down, asleep or not
  yet started, those runs don't happen.
- A run is skipped while the previous run of the same automation is still
  working, with a maximum runtime after which the old run stops blocking.
- The newest N tabs per automation are kept (default 5), and older finished
  ones are closed.
- Default commands run fully unattended:
  - Claude Code: `claude --dangerously-skip-permissions`
  - Codex: `codex --dangerously-bypass-approvals-and-sandbox`
- A custom command is used as typed, with the prompt appended as its last
  argument, for example `cx --print '<prompt>'`. Making a custom command work
  is the user's responsibility.
- No prompt appendix or system text is added. The prompt is passed verbatim.
- Agents may create and edit automations by writing the JSON file directly.
  There is no CLI.
- Mobile gets no automations UI. The automation workspace already shows up
  there as an ordinary workspace.
- Reboots: no systemd or launchd service. This doc says that automations run
  while the helper is running and resume after the next connection.
- Built plugin-style, so the feature can be removed cheaply if it isn't used.

## Out of scope

- Precheck commands (an agent can be told to stop when there's nothing to do).
- Missed-run catch-up and grace windows.
- Starting the helper at boot.
- A new git worktree per run.
- Event triggers (GitHub, webhooks, Slack).
- Editing on mobile.
- A command-line interface.
- Usage and cost tracking.

## Storage

Everything lives in the helper's existing state directory, `paths::state_dir()`
(`apps/host/src/paths.rs`), the directories listed in
[Using automations](#using-automations).

| Path | Written by | Contents |
|---|---|---|
| `automations.json` | People and agents; in phase 2 also the desktop, through the helper. The scheduler only reads it. | Definitions |
| `automations/runs.json` | Helper only | Run history, and the errors found in the definitions |
| `automations/prompts/<id>-<run>.txt` | Helper only | The prompt for each run, deleted once the run finishes |

Keeping definitions and run state in separate files means the scheduler never
writes the file that people and agents edit, so the two can't collide.
`runs.json` is written with the existing atomic pattern
(`snapshot::save_sidecar`): a 0600 temp file, write, `sync_all`, rename, then
fsync the parent directory. Prompt files are plain 0600 writes.

### `automations.json`

| Field | Required | Rules |
|---|---|---|
| `version` | yes | `1`. Any other value: the file is reported as unreadable. |
| `id` | yes | `[a-z0-9-]{1,64}`, unique in the file. Used in tmux tags and file names, so it is stable across renames. If two automations share an id, neither runs. |
| `name` | yes | 1–100 bytes, no control characters. Used in tab names. |
| `enabled` | no, default `true` | |
| `schedule` | yes | Cron, see [Cron dialect](#cron-dialect). |
| `cwd` | yes | Absolute or `~/…`. `~` is expanded from the helper's `$HOME`, as for new workspaces. Must exist, be a directory and be enterable when the run starts. |
| `agent` | yes | `claude`, `codex` or `custom`. |
| `command` | only when `agent` is `custom` | Non-empty, one line. Typed as is. Ignored for `claude` and `codex`. |
| `prompt` | yes | Non-empty, up to 64 KiB. |
| `keepTabs` | no, default 5 | 1–50. |
| `maxRuntimeMinutes` | no, default 120 | 5–1440. After this long a run no longer blocks the next one, and once it is older than the newest `keepTabs` its tab may be closed even if it is still running, unless an agent in it is reported busy by hooks. |

Unknown fields are ignored, so a newer UI's file stays readable.

Loading:

- The scheduler re-reads the file whenever its device, inode, size or mtime
  changes, checked on every tick.
- If the file can't be parsed or has the wrong `version`, the helper keeps the
  last set that loaded and records the error, with serde's line and column, as
  `fileError` in `runs.json`.
- If the file parses but one automation is invalid, that one is skipped and its
  error is recorded as that automation's `definitionError`. The others still
  run.
- A missing file means there are no automations, and the feature is idle.

### `automations/runs.json`

```json
{
  "version": 1,
  "fileError": "",
  "automations": {
    "fix-ci": {
      "definitionError": "",
      "runs": [
        {
          "number": 12,
          "scheduledFor": "2026-10-13T09:00",
          "startedAt": 1791882000000,
          "finishedAt": 1791882780000,
          "status": "done",
          "sessionId": "$4",
          "windowId": "@41",
          "paneId": "%87",
          "serverIdentity": "tmux:/tmp/tmux-1000/default:…",
          "lastMessage": "CI on main is green. Nothing to fix.",
          "error": ""
        }
      ]
    }
  }
}
```

- `status` is one of `launched`, `working`, `needs_attention`, `done`, `closed`
  (the tab went away before the run finished), `failed` (it could not start;
  see `error`) and `skipped` (the previous run was still working).
- Each automation keeps its newest 100 finished runs. Unfinished runs are never
  pruned.
- `lastMessage` is the agent's final message, limited to 4 KiB.
- The next run number is one more than the highest recorded, or than the
  highest on an open tab, whichever is larger. Numbers keep going up even when
  history is lost.
- History for an automation removed from `automations.json` is deleted on the
  next successful load once none of its runs is unfinished, never while the
  file is unreadable. A file that is missing counts as empty; one that can't
  be read counts as unreadable.
- An unreadable `runs.json` starts over empty rather than stopping automations.

## Scheduler

The scheduler is a tokio task started through the host plugin list (see
[Plugin structure](#plugin-structure)) next to the daemon's 2-second
`hook_retry` timer. It ticks every 15 seconds, on a blocking thread. Each tick:

1. Reloads `automations.json` if it changed, and copies its errors into
   `runs.json`.
2. Updates every unfinished run's status (see [Status tracking](#status-tracking))
   and files final messages. Doing this before firing means the overlap check
   sees fresh statuses.
3. Works out which Unix minutes have started since the last tick.
   - At daemon start, the cursor is set to the current minute, so nothing that
     came due earlier fires.
   - If more than 2 minutes passed since the previous tick *ended* (the host
     was asleep or the process stalled), those minutes are skipped. Measuring
     from the end means a slow tick is never mistaken for a sleep.
4. For each such minute, starts a run of every enabled automation whose cron
   matches that minute's local time, unless the automation's history already
   has a run for the same local minute. Iterating Unix minutes and comparing
   local keys means each minute of the hour daylight saving time repeats fires
   once, and the hour it skips never matches.
5. Closes old tabs beyond `keepTabs` for automations that just started a run.
6. Saves `runs.json` if anything changed.

A tick makes no tmux call at all when no automation is enabled and no run is
unfinished.

### Cron dialect

The parser is hand-written and adds no dependencies.

- Five fields: minute, hour, day of month, month, day of week.
- Each field accepts `*`, numbers, `a-b` ranges, `,` lists and `/n` steps. A
  single value with a step, `5/20`, runs from that value to the end of the
  field.
- Three-letter month names (`jan`–`dec`) and day names (`sun`–`sat`) are
  accepted, in any letter case.
- Day of week `0` and `7` both mean Sunday.
- Vixie rule: when both day of month and day of week are restricted, a day
  matches if either one matches. A field starting with `*`, including `*/2`,
  counts as unrestricted.
- Not supported: `@hourly`-style macros, `L`, `W`, `#`, `?`, and seconds.
- A schedule that never fires within 5 years is rejected (for example
  `0 0 31 2 *`).
- Local time comes from `localtime_r`, after `tzset`, so it follows the host's
  time zone, including a zone changed while the helper runs.

## Running an automation

### The automation workspace

- The helper looks for the session tagged with the session option
  `@muxflow-automation-workspace` set to `1`, so renaming the session doesn't
  break anything.
- If there is none, it creates one named `automation`, or `automation-2`, … if
  an untagged session already has that name. It uses the same session setup as
  `CreateSession` (`configure_new_session`), and the new session's first tab
  becomes the run's tab.
- When the helper is what starts the tmux server, the session gets the
  embedded-Codex marker the same way `CreateSession` sets it.
- All tmux calls go through `snapshot::tmux_command()`, so they use the same
  tmux server as everything else.

### Starting a run

1. Check that `cwd` exists, is a directory and can be entered
   (`resolved_start_directory`). If not, record the run as `failed`.
2. Write the prompt to `automations/prompts/<id>-<n>.txt`.
3. Create the tab in the background so it doesn't take focus:
   `new-window -d -P -F "#{window_id} #{pane_id}" -t <session>: -c <cwd> <APP_SHELL>`.
4. In one tmux call, name the tab `<name> #<n>` and set its window options:
   `@muxflow-automation <id>`, `@muxflow-automation-run <n>`,
   `@muxflow-name-locked 1`, `automatic-rename off`, `allow-rename off`.
5. Wait for the shell to draw anything (`capture-pane`, every 100 ms, up to
   10 seconds), so shells that drop typeahead while starting don't lose the
   command.
6. Type the command line through the existing terminal paste path
   (`send_input_batch`, bracketed paste with an `ade-input-` buffer the
   clipboard forwarder ignores), then send `Enter`.

If naming or typing fails, the tab is closed again and the run is `failed`.

The command goes through the user's interactive shell, so aliases and shell
functions such as `cx` work. This matches the mobile agent command.

### Command line

```
<command> "$(cat '<state_dir>/automations/prompts/<id>-<n>.txt')"
```

| `agent` | `<command>` |
|---|---|
| `claude` | `claude --dangerously-skip-permissions --` |
| `codex` | `codex --dangerously-bypass-approvals-and-sandbox --` |
| `custom` | the `command` field, unchanged |

The `--` keeps a prompt that starts with `-`, such as a bullet list, from
being read as an option.

Reading the prompt from a file keeps the typed line short and gives the
command exactly one argument, the prompt, however many lines and quotes it
has. The path is single-quoted. fish treats `\` inside single quotes as an
escape, so a path containing `'` or `\` is refused rather than escaped; the
default state directories contain neither. fish needs version 3.4 or later
for `$(…)`.

The default commands start an interactive session with the prompt already
submitted. They keep running after the agent answers, so the user can open the
tab and continue the conversation.

### Status tracking

Each tick, every unfinished run whose command was typed at least 5 seconds ago
gets a status from its pane. Younger runs are left alone, because their shell
may still be in the foreground before the command starts. A run's `startedAt`
is when its command was typed.

| Observation | Status |
|---|---|
| The pane is gone, or tmux is now a different server | `closed` |
| An agent record on the pane is Blocked, or its `attention_kind` is `blocked` | `needs_attention` |
| That agent is Working | `working` |
| That agent is Idle with `attention_kind` `completed`, changed after the command was typed | `done` |
| That agent is Idle otherwise | unchanged |
| No agent record (or lifecycle Unknown), and the pane's shell is its terminal's foreground job again | `done` (the command exited) |
| No agent record, and another job is in the foreground | `working` |

- `done`, `closed`, `failed` and `skipped` are final.
- Agent records come from `AgentRuntime::snapshot_for(server identity)`, only
  while some run is unfinished, and when closing old tabs.
- The foreground check reads the process table (`ps -o pgid=,tpgid=` for the
  pane's shell), not tmux's command name, which says `bash` while a bash
  script runs.
- If tmux can't be asked, or answers without a server identity, statuses stay
  as they are rather than reading as closed. Only "no server running" closes
  every run.

The final message: the agent runtime's reply sink (`service/agents/mod.rs`)
hands every `Stop` hook's `last_assistant_message` to voice mode and to the
host plugins. The sink runs while ingest holds its ordering lock, so the
automations plugin only pushes it onto a bounded in-memory queue (64 replies).
The next tick files each reply on the run with the same pane and server.

### Overlap

When a scheduled run comes due, an earlier run of the same automation still
blocks it if:

- its status is `launched`, `working` or `needs_attention`, and
- it started less than `maxRuntimeMinutes` ago.

Then the new run is recorded as `skipped` with "#<n> was still working" and no
tab is created. A row of skips behind the same run shares one record, which is
updated to the latest skipped minute, so a long run doesn't use up run numbers
or crowd the history. A tab that closed has already been marked `closed` by the time
this check runs.

- `needs_attention` counts as active, so a stuck run doesn't pile up new ones
  behind it.
- `maxRuntimeMinutes` is the backstop for a status that never clears, such as
  an agent without hooks or a question nobody answers.

### Closing old tabs

After an automation starts a run, the helper lists panes with their tags and,
for that automation:

- considers only tabs still alone in the automation workspace: a tab the user
  split or moved to another workspace is theirs now and is never closed
- keeps the `keepTabs` highest run numbers
- kills an older window only if its run is final, past `maxRuntimeMinutes`, or
  has no record at all
- never kills a window where an agent is working or waiting on someone, so a
  conversation picked up again in an old tab survives

A tab the user closes by hand before its run finished marks the run `closed`
on the next tick. There's
no hard tmux limit on window count; the limit is memory, since every idle agent
session stays resident.

### Tab names

- Run identity always comes from the window tags and IDs, never the name.
- The name `<name> #<n>` stays put because tmux renaming is off for that
  window. A `pane-title-changed` hook of the user's own that calls
  `rename-window` can still rename it.
- The opt-in host-naming hook (`service/tmux_config.rs`) renames windows from
  agent pane titles. Its guard condition includes
  `#{==:#{@muxflow-name-locked},}`, so it skips any window with the lock tag.
  The tag is general on purpose: it is the first step of T053, and it stays if
  automations are removed.
- Because the guard changed, a host whose hook was already applied gets it
  re-applied once (`host-naming` reports `applied`, and the desktop shows its
  usual "applied" status line).

## Plugin structure

There are two general extension points. They stay even if automations is
removed.

### Host plugin list

`apps/host/src/service/plugins/mod.rs` has two plain functions:

- `start()` returns the plugins' daemon-lifetime tasks. `daemon.rs` starts them
  after `hook_retry` and aborts them beside it.
- `on_agent_reply(&AgentReply)` is called from the agent runtime's reply sink,
  next to `voice::on_agent_reply`. `ingest.rs` is untouched.

The plugin lives under `service/` so it can use `service` internals with small
visibility changes: `snapshot::{load_sidecar, save_sidecar,
server_identity_from_command}`, the `tmux_actions` command helpers, and
`terminal::send_input_batch`.

### Desktop right-panel extensions

`apps/desktop/src/features/shell/panelExtensions.ts`:

```ts
export interface PanelExtension {
  id: string;                  // "automations"
  label: string;               // "Automations"
  settingLabel: string;        // "Show Automations"
  settingHint: string;
  Component: ComponentType<PanelExtensionProps>;
}
export const PANEL_EXTENSIONS: readonly PanelExtension[] = [/* automations */];
```

`PanelExtensionProps` gives an extension the shown hosts (profile, label, live
client id, connected), the active host, `onOpenPane` and `onMessage`.

The built-in places read this list:

- **Saved settings.** `src-tauri/src/app_state.rs` stored the panel as
  `enum PanelSurface { Files, Git, Ports }`, where an unknown value failed the
  whole load. It is now a string defaulting to `"files"`, plus
  `enabled_panel_extensions`. The contract (`persistedAppState.contract.json`)
  and its tests changed with it.
- `PanelSurface` in `types.ts` is a string. Normalization keeps a built-in
  surface or an enabled extension's, and falls back to `files` otherwise.
- `RightPanel` and `AppRightPanel` render the enabled extensions after the
  built-ins; arrow-key cycling uses the visible list, and a surface that isn't
  showable displays Files.
- The Settings Workspace tab renders one checkbox per extension.
- There are no `view.show*` commands for extensions, so the command registry
  stays static. Selecting an extension's tab opens the panel on it.

A new panel is one entry in the list.

## Desktop panel (phase 2)

### Wire

A new `crates/protocol/proto/automation.proto` in the same package
(`tmux_agent.protocol.v1`), imported by `envelope.proto` and compiled by
`crates/protocol/build.rs` (`rerun-if-changed=proto`). Changes to
`envelope.proto`:

- `OPERATION_AUTOMATION = 58`
- `Request.automation = 24`, `Response.automation = 15`
- `EVENT_KIND_AUTOMATION = 22`, `HostEvent.automation = 14`

Requests:

- **List** returns every entry of `automations.json` as written, invalid ones
  included with their error (or, while the file can't be read, the entries of
  the last version that loaded, beside `file_error`), plus each one's next run, last 20 runs and the
  line typed into its tabs, and `file_error`, `generation` and `state_dir`. It
  also picks up an edit the next tick would have, and subscribes the connection
  to automation events.
- **Save** and **Delete** carry `base_generation`, a blake3 hash of the file's
  contents (empty for no file). A mismatch fails with `stale_generation` and
  the UI reloads, so a UI save never overwrites an agent's edit. An id the
  file uses twice is refused (`duplicate_id`) rather than guessed. Save validates
  like loading, plus that `cwd` exists. The file is edited as JSON so unknown
  fields survive, written pretty-printed and atomically
  (`snapshot::write_private_atomic`), and reloaded at once. A file that can't
  be read as a list of automations is refused (`file_unreadable`) rather than
  replaced, so a half-finished hand edit is never lost.
- **RunNow** starts a run through the scheduled path, without the overlap
  check. Its `scheduledFor` is empty, so it never stands in for a scheduled
  minute.
- **PreviewSchedule** returns a schedule's error or its next three run times.

`AutomationEvent { bool changed }` tells subscribed connections to List again.
Events go only to connections that listed (`send_control_event_to`), never as a
broadcast. They follow a tick that changed something, a reload of the file,
and every Save, Delete and RunNow. A subscriber whose queue was full when a
tick changed something hears on the next tick; one that missed an edit's event
lists again on the next change, and its own saves are still checked against
the generation. One that has gone is dropped.

Host plumbing: `Handler::Automation` in `operation_policy.rs` (control lane,
detached, plus a row in the exhaustive table), one arm in `dispatcher.rs` that
runs `plugins::automation_request` on a blocking thread, and
`plugins/automations/dispatch.rs`.

Compatibility:

- Desktop and helper change in the same commit, as usual.
- Older mobile against the new host: it never sends `OPERATION_AUTOMATION` and
  never subscribes, so it gets no automation events. No break.
- Newer mobile against an older host: mobile doesn't use the operation. No
  break.
- `PROTOCOL_MAJOR` stays at 4. The mobile generated types, including the new
  `automation_pb.ts`, are regenerated and committed in the same change.

### Desktop

- Bridge: `src-tauri/src/connection/automation.rs`, an `automation_request`
  command modeled on `connection/agent.rs`. The event goes through `bridge.rs`
  and `event_frame.rs` (kind byte 19, no payload), is decoded in
  `features/terminal/api.ts`, and `useAppConnectionController.ts` hands it to
  `publishAutomationChanged` for every host, like agent events, not only the
  active one like git events.
- `apps/desktop/src/features/automations/`: `AutomationsPanel.tsx` (list,
  detail and history), `AutomationEditor.tsx`, `api.ts`, `types.ts`,
  `cronText.ts`, plus tests.
- The panel is host-scoped, using each `HostLink`'s client, the local host
  included. Only the host chooser is copied from Ports, which talks over SSH
  directly.
- Mock 1, the list: name, last-run chip, schedule in plain English plus next
  run, agent or command, the enable switch, and Run now. A "New" button.
- Mock 2, the editor: name; Claude Code / Codex / Custom command; command
  (read-only for the presets); prompt; working directory; cron with its
  plain-English reading and next three runs; keep tabs; overlap limit; a "What
  runs" preview; Delete, Cancel, "Save and run now", Save.
- Mock 3, detail and history: each run's number, start, duration, status chip
  and final message, plus "Open tab" while the window exists. Open tab follows
  the agent row's path (`selectOnHost` and `resolveTerminalDestination`).
- Mock 4, edge states: empty (shows the file path), the file can't be read
  (banner with the error over the last version that loaded, which is what
  still runs), host disconnected (the reboot note). One change from the mock:
  while the file can't be read, the list is read-only and saving is refused
  instead of replacing the file, which would throw away every other automation
  in it. The banner says to fix the file on the host. Run now still works.
- Hidden unless "Show Automations" is checked in Settings, Workspace tab.

### Mobile

No feature work. Regenerate the protocol types only.

## Removal

If the feature isn't used:

1. Delete `apps/host/src/service/plugins/automations/` and its lines in
   `service/plugins/mod.rs`. Remove the re-exports that only it used
   (`APP_SHELL`, `resolved_start_directory` and the others in
   `tmux_actions.rs`, `send_input_batch` in `terminal.rs`), or clippy reports
   them unused.
2. Delete `apps/desktop/src/features/automations/`, its entry in
   `PANEL_EXTENSIONS`, and the `.automation*` rules in `styles.css`.
3. Delete `crates/protocol/proto/automation.proto` and its line in
   `crates/protocol/build.rs`. In `envelope.proto`, remove the import and the
   five additions, and reserve their numbers. Remove the `operation_policy`
   entry and the `dispatcher` arm.
4. Delete `src-tauri/src/connection/automation.rs`, its `generate_handler!`
   entry, the event mapping in `bridge.rs` and `event_frame.rs`, the decode in
   `features/terminal/api.ts` and its test, and the `automationChanged`
   branch in `useAppConnectionController.ts`.
5. Regenerate the mobile protocol types.
6. Delete this file, `docs/design/automations-mock.html`, and their links.

What stays: the host plugin list, the right-panel extension list, the
string-typed saved panel, and the `@muxflow-name-locked` hook guard. All are
general and harmless. Files left on hosts (`automations.json`, `automations/`)
and the `automation` tmux session are inert.

## Validation (phase 0)

Done before phase 1, on Linux with Claude Code 2.1.296 and Codex, each typed
into a fresh directory in a scratch tmux session through bracketed paste:

- Neither default command stopped at a folder-trust, bypass or hook-trust
  prompt. Both answered a two-line prompt containing both quote styles
  verbatim.
- Both agents' Stop hooks fired: their records reached Idle with
  `attention_kind` `completed`.
- Only bash was available; zsh and fish typing is untested.

## Risks and open questions

- **Unattended prompts:** a future agent version may add a first-run
  confirmation. The run then waits in its tab and blocks the next runs until
  `maxRuntimeMinutes`.
- **Fully unattended agents:** they run with the user's full permissions and,
  for Codex, without a sandbox. This is the user's explicit choice.
- **Shell startup:** some prompt frameworks drop typeahead. The wait for the
  first prompt covers the common case; zsh and fish are untested.
- **Environment:** the tab's shell is started by the tmux server and gets that
  server's environment plus the user's rc files, the same as any Muxflow tab.
- **Runs without hooks:** status falls back to "is a job other than the shell
  in the foreground". The default agents, and any custom command that opens an
  interactive program, read as `working` until they exit. The overlap check
  and tab cleanup stop waiting for them after `maxRuntimeMinutes`.
- **Notifications:** every finished agent run raises the usual "completed"
  alert on desktop and mobile, like any other agent.
- **Panel requests during launches:** a launch holds the scheduler while it
  waits, up to 10 seconds, for the new tab's shell to draw. A shell that never
  draws a prompt delays panel requests by that long per run starting.
- **Helper upgrades:** the daemon restarts when a different desktop build
  connects. A minute that came due during the restart is skipped.
- **Saved settings across builds:** a desktop build from before the
  panel-extension change can't read a saved extension panel. Builds ship in
  lockstep, but a developer running `pnpm tauri dev` beside an installed older
  build should back up the app state first.
