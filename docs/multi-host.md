# Multi-host desktop: design contract

Show workspaces and agents from several hosts at once in the desktop sidebar,
each tagged with a one-letter host mark. The wire protocol and the host helper
do not change; mobile is untouched. Only the desktop (React + Tauri) changes.

## Vocabulary

- **Profile**: a saved host (`HostProfile`). Local is the profile `local`.
- **Shown**: a profile the user checked. Shown hosts get a live bridge and
  appear in the sidebar. The active host is always shown.
- **Active**: the one host whose workspace is on screen (terminal, tabs, files,
  git). Persisted as `lastProfileId`. Exactly one at a time.
- **Letter**: the host mark drawn before every workspace row, agent row and
  ⌘P row. Default: first character of the label, upper-cased (`L` for Local).
  Drawn only when two or more hosts are shown.

## Profiles (Rust + TypeScript)

```ts
interface HostProfile {
  id: string;
  label: string;
  connection: ConnectionSpec;
  /** One character. Absent means "derive from the label". */
  letter?: string;
  /** Checked in the host chooser. Absent means false. */
  shown?: boolean;
}
```

- Rust `HostProfile` gains `letter: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`,
  validated: trimmed, exactly one character when present) and `shown: bool` (`#[serde(default)]`).
- `save_host_profile` no longer moves `last_profile_id`.
- New command `set_last_profile_id(profile_id)`; refused for an unknown id.
- `hostLetter(profile: Pick<HostProfile, "id" | "label" | "letter">): string` in
  `features/shell/hostProfiles.ts`: `letter` when present, else the first
  character of the trimmed label, else the first character of the id; always
  upper-cased.
- `hostProfileId(connection)` stays the identity used everywhere.

## Bridges

- `start_terminal` gains `attach: bool`. With `attach=false` the bridge sends
  the topology snapshot, the agent snapshot and every ordered event, but sends
  no AttachTerminal and seeds no pane, on the first connect and on every
  supervisor reconnect, until `select_terminal_session` names a session. From
  then on it behaves exactly like an attached client, reconnects included.
- TS: `startTerminal(scope, connection, attach, onEvent)`.
- One bridge per shown host, all alive at once. The active host is a pointer.
  Switching hosts flips the pointer and calls `selectTerminalSession` on that
  host's client; it never restarts a bridge.
- A host that stops being shown has its bridge stopped and its file watches
  retired (`fileClient.retireConnection(clientId)` moves from the clientId
  effect in App.tsx to the link's stop path).

## Host links (frontend state)

`state/hostLinks.ts` (new): a reducer over

```ts
interface HostLink {
  profileId: string;
  connection: ConnectionSpec;
  /** Renderer incarnation; a bump restarts this host's bridge. Never repeats for a profile in one process. */
  connectionEpoch: number;
  clientId?: string;
  /** Native GenerationEpoch of the live bridge; 0 before the first one. */
  terminalEpoch: number;
  hostState: NormalizedHostState;
  /** Remembered per host so switching back lands where the user left. */
  activeSessionId?: string;
  activeWindowId?: string;
  detail: string;
}
interface HostLinksState { order: string[]; byProfileId: Record<string, HostLink> }
```

`useAppConnectionController` keeps its returned fields for the active host
(`hostState`, `snapshot`, `clientId`, `clientIdRef`, `hub`, `terminalEpoch`,
`connectionEpoch`, `currentHostScope`, `activeSessionId`, `setActiveSessionId`,
`setConnectionEpoch`, …) so downstream hooks do not change. It adds:

- `links: readonly HostLink[]` in profile order, shown hosts only
- `activeProfileId`, `activateHost(profileId)`, `reconnectHost(profileId)`
- `hubFor(profileId)`, `linkFor(profileId)`

Telemetry (echo lag probe, input latency reporter, link quality, host latency)
stays a single instance bound to the active client id. Resume recovery probes
the active client and, on failure, bumps every link's epoch. Flow-stall and
server-identity resyncs bump only the affected link.

## Terminal state cache

Pane ids repeat across hosts (`%0` exists on every tmux server). The cache key
is `terminalCacheKey(scope, paneId)` = `${scope}\0${paneId}` where `scope` is
the host profile id. `TerminalPane` and `TerminalWorkspaceSurface` take a
`cacheScope: string` prop. `TerminalStateCache.clearScope(scope)` replaces
whole-cache clears that were really "this host's bridge restarted". The
terminal surface is keyed by the active profile id so panes remount on a host
switch.

## Merged rows

`features/workspaces/mergedWorkspaceRows.ts` (new, pure):

```ts
interface HostRowSource {
  hostProfileId: string;
  letter: string;
  label: string;
  phase: ConnectionPhase;
  canMutate: boolean;
  transport: "local" | "ssh";
  scope: HostScopeToken;
  snapshot: TmuxSnapshot;
  /** Only the active host has one. */
  activeSessionId?: string;
  agents: readonly AgentRecord[];
  adapters: readonly AgentAdapterDescriptor[];
  attentionByWorkspace: ReadonlyMap<string, AgentAttentionRollup>;
  activeBranch?: string;
  home?: string;
}
interface MergedWorkspaceRow extends WorkspaceRowModel {
  key: string;            // `${hostProfileId}\0${session.id}`
  hostProfileId: string;
  letter: string;         // "" when letters are hidden
  scope: HostScopeToken;
  canMutate: boolean;
}
function mergedWorkspaceRows(sources: readonly HostRowSource[], showLetters: boolean): MergedWorkspaceRow[];
function pinnedOnlyMergedRows(rows, active: { hostProfileId: string; sessionId?: string }): MergedWorkspaceRow[];
```

Order: every pinned row first (sources in order, each host's own order inside),
then the rest the same way. ⌘1–⌘9 and ⌘P read this list. Per-host
`workspaceRows` results are memoized per host by the caller so one host's
event never recomputes another host's rows.

Agent rows: `AgentLocation` gains `hostLetter?: string`. The sidebar draws it
before the agent name the way it draws the workspace letter. Nothing changes on
tabs.

## Sidebar

`WorkspaceSidebar` takes `hosts: readonly SidebarHost[]` instead of the single
`hostLabel/transport/phase/latencyMs/commandScope/canMutate`:

```ts
interface SidebarHost {
  profileId: string; letter: string; label: string; transport: "local" | "ssh";
  phase: ConnectionPhase; canMutate: boolean; scope: HostScopeToken;
  active: boolean; shown: boolean; latencyMs?: number;
}
```

Row callbacks carry the row (which carries its host scope) rather than a
session plus the sidebar-wide scope: `onSelectWorkspace(row)`,
`onTogglePinnedWorkspace(row)`, `onWorkspaceCommand(row, commandId)`. Agent
callbacks keep `(row, scope)` with the scope looked up from the row's host.

The host row at the bottom shows the active host as today. Clicking it opens
a `ContextMenu` with one `checked` item per saved profile (toggle shown; the
active host's item is disabled-checked), a separator, and "Connection
settings…". Activation of another host happens by clicking one of its
workspace or agent rows. Settings › Connection gets a "Letter" field and a
"Show in sidebar" checkbox on the profile form.

## Agents

`useAgentRuntime` becomes multi-host: state is keyed by host profile id, it
takes `scopes: readonly AgentRequestScope[]` (one per shown host with a live
client) plus the active `focus`, requests one snapshot per scope, decides
notifications and sounds for every host, and acknowledges seen only for the
focused pane on the active host. It returns the active host's `agents`,
`adapters`, `rollups` as today plus `byHost: ReadonlyMap<string, { agents; adapters; rollups }>`.
Launch, rename, resume and hook actions use the active host's scope.

## Cross-host actions

- Selecting a workspace or agent row on another host: `activateHost(profileId)`
  then `selectSession(sessionId)` once the facade reports that host (an effect
  keyed on a pending `{ profileId, sessionId, paneId? }`).
- Bell jump and notification click use the same path.
- Row mutations (pin, rename, move, close, agent tab pin, agent rename) run
  against the row's host: `useTmuxActionPerformer` accepts an optional target
  `{ clientId, canMutate, scopeRef }`, resolved from the row's `hostProfileId`
  via `linkFor`; `useShellCommands` resolves a targeted command's host through
  `hostForScope(scope)`, and a close confirmation carries the scope it was
  opened for. An agent resume opens a new pane beside the one on screen under
  the explorer's active root — facts of the active host — so a peer's agent is
  resumed after its host is activated; the row says so.
- Settings › Connection's "Letter" and "Show in sidebar" edit the picked saved
  host and save on change (`save_host_profile`), without Connect. The host on
  screen cannot be un-shown from either place.
- Focus history is scoped to the active host and cleared on a host switch.
- `DisconnectedStrip` reports the active host only; peer phases show as the
  dot in the host menu and as dimmed rows.

## Out of scope

Tabs get no letters. The titlebar is unchanged. Mobile multi-host is a later
phase.
