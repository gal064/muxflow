import type { ConnectionSpec } from "../../app/types";
import { isAgentSortMode, type AgentSortMode } from "../agents/agentsList";
import { isMarkdownAppearance, isThemePreference, type MarkdownAppearance, type ThemePreference } from "../../ui/appearance";

export type AppTabKind = "file" | "markdown" | "gitDiff";
/** How a Markdown tab is drawn; also the shape of the persisted default. */
export type AppTabViewMode = "source" | "live" | "preview" | "split";
/** The right panel's own segments; they share one 300px surface. */
export const BUILT_IN_PANEL_SURFACES = ["files", "git", "ports"] as const;
/**
 * A built-in segment, or the id of a panel extension (`panelExtensions.ts`).
 * A string so an extension adds no case here.
 */
export type PanelSurface = string;

/** Sidebar geometry, from the plan's token table (cmux). */
export const SIDEBAR_MIN_WIDTH = 240;
/** The interface sizes Settings offers, as zoom factors of the whole window. */
export const UI_SCALES = [0.9, 1, 1.1, 1.2, 1.3] as const;
/** Fraction of the window a dragged sidebar may not exceed. */
export const SIDEBAR_MAX_WINDOW_FRACTION = 1 / 3;
/** Right panel geometry; the same rules as the sidebar, from its own edge. */
export const PANEL_MIN_WIDTH = 240;
/** Fraction of the window a dragged panel may not exceed. */
export const PANEL_MAX_WINDOW_FRACTION = 1 / 2;
/** The terminal width (about 60 columns) a rail may not squeeze below; past it the rail hides. */
export const TERMINAL_MIN_WIDTH = 480;
/** Share of the workspace a docked document takes by default, and the bounds a drag stays within. */
export const DOCK_DEFAULT_FRACTION = 0.4;
export const DOCK_MIN_FRACTION = 0.2;
export const DOCK_MAX_FRACTION = 0.7;
/** Share of the sidebar's height the agents section takes by default. */
export const AGENTS_SECTION_DEFAULT_RATIO = 0.42;
export const AGENTS_SECTION_MIN_RATIO = 0.15;
export const AGENTS_SECTION_MAX_RATIO = 0.75;
export const TERMINAL_FONT_SIZE_MIN = 10;
export const TERMINAL_FONT_SIZE_MAX = 20;
/** The same ceiling `hostSetup` uses: one entry per host profile, not per whim. */
export const MAX_WORKSPACE_DEFAULT_HOSTS = 1_024;
/**
 * The longest a workspace default may be, in characters.
 *
 * Enforced on *this* side because the storage side's limit is a refusal of the
 * whole save: `save_app_state` validates every text field against 16 KiB and
 * returns an error for the entire state, so one over-long paste into a settings
 * field would stop open tabs, workspace selection, shortcut overrides and window
 * geometry from persisting at all. Well under that ceiling even at four bytes
 * per character, and far past any real path or command.
 */
export const MAX_WORKSPACE_DEFAULT_LENGTH = 2_048;

export interface AppOwnedTab {
  id: string;
  hostProfileId: string;
  serverIdentity: string;
  sessionId: string;
  sessionName: string;
  kind: AppTabKind;
  resource: string;
  title: string;
  order: number;
  rootPath?: string;
  rootToken?: string;
  /**
   * VS Code's preview tab: opened by a single click, italic in the strip, and
   * the one slot the next single click reuses instead of adding a tab. Cleared
   * — "pinned" — by a double-click, by Enter, or by the first edit.
   *
   * Absent rather than `false` when pinned: this crosses the `save_app_state`
   * boundary, and an absent optional is what every other optional here does.
   */
  preview?: boolean;
  viewMode?: AppTabViewMode;
  /** Git diff tabs retain opaque host path and repository identity. */
  gitRepositoryId?: string;
  gitPath?: string;
  gitOriginalPath?: string;
  gitTarget?: "staged" | "unstaged";
  gitStatusGeneration?: string;
  gitSourceGeneration?: string;
}

export interface WorkspaceUiRecord {
  hostProfileId: string;
  serverIdentity: string;
  sessionId: string;
  sessionName: string;
  selectedAppTabId?: string;
  /**
   * The one document shown beside the terminal rather than over it. Never the
   * selected tab: selecting covers the terminal column, docking sits next to it.
   */
  dockedAppTabId?: string;
  /** Which edge of the workspace the docked document sits on; absent when nothing is docked. */
  dockSide?: DockSide;
}

export type DockSide = "left" | "right";

export interface ShellState {
  /** Which segment of the right panel is showing when it is open. */
  panelSurface: PanelSurface;
  /** The right-panel extensions turned on in Settings, by id. */
  enabledPanelExtensions: string[];
  /** ⌘B. Collapsed means 0px, not a 44px icon strip. */
  sidebarCollapsed: boolean;
  /** Drag-resizable; clamped against the window at render time. */
  sidebarWidth: number;
  /** ⌥⌘B. Closed by default, and it reserves no space when closed. */
  panelOpen: boolean;
  /** Drag-resizable from its left edge; clamped against the window at render time. */
  panelWidth: number;
  /** A docked document's share of the workspace width, shared by every workspace. */
  dockFraction: number;
  /** The agents section's one control. */
  agentSort: AgentSortMode;
  /** Position of the sidebar's internal divider, as a share of its height. */
  agentsSectionRatio: number;
  /** Draws a shape inside each state dot as well as coloring it. */
  agentStateGlyphs: boolean;
  /** Hides per-agent activity lines from workspace rows. */
  compactWorkspaces: boolean;
  /** Puts the tab strip in the title bar, beside the window controls. */
  tabsInTitleBar: boolean;
  /** Focuses a terminal pane when the pointer rests on it, without a click. */
  focusFollowsMouse: boolean;
  /**
   * The workspace list's one filter: only pinned workspaces are listed, and
   * the agents section shows only their agents.
   *
   * Pins already say which workspaces matter, so this needs no second concept
   * — it is a boolean over the pins the host reports. The
   * workspace on screen is the one exception the list makes, so turning the
   * filter on can never leave the shell showing something the sidebar denies.
   */
  pinnedOnly: boolean;
  /**
   * Makes terminal *content* readable to a screen reader. Off by default
   * because xterm's screen-reader mode costs a string allocation and an
   * emitter dispatch per printed codepoint (P12-U002); this is the setting
   * that Phase 12 deferred to Phase 11's settings surface.
   */
  terminalScreenReader: boolean;
  /** Copies a completed, non-empty terminal selection to the system clipboard. */
  copyOnSelect: boolean;
  /** Joins only high-confidence display-wrapped shell commands before copying. */
  cleanWrappedCommands: boolean;
  /** Allows terminal programs to write the system clipboard through OSC 52. */
  terminalApplicationClipboard: boolean;
  /** Terminal text size in integer CSS pixels. */
  terminalFontSize: number;
  /** Light, dark, or whichever the OS is using. */
  theme: ThemePreference;
  /** Zoom of the whole interface, terminals included; 1 is 100%. */
  uiScale: number;
  /** Markdown's Live and Preview colors: the app's, or always light or dark. */
  markdownAppearance: MarkdownAppearance;
  /** The host most recently picked in the New workspace dialog. */
  newWorkspaceHostProfileId?: string;
  /**
   * The mode a *newly opened* Markdown tab starts in.
   *
   * Read once, when the tab is created. A tab that is already open keeps
   * whatever mode it is in, and switching one tab's mode never rewrites this:
   * the setting is the starting point, not a mirror of the last tab touched.
   */
  defaultMarkdownView: AppTabViewMode;
  /**
   * Reopen the remembered workspaces when a host's tmux server comes back
   * without them. Off until asked: recreating terminals nobody opened is a
   * decision only the user gets to make on their machine.
   */
  restoreWorkspaces: boolean;
  /** Physical geometry plus the capture scale, used to preserve logical size across monitors. */
  windowGeometry?: { x: number; y: number; width: number; height: number; maximized: boolean; scaleFactorMilli?: number };
}

/**
 * The user's one-time answer to "set up agent status on this host".
 *
 * Recorded per host profile, because agreeing to merge hook entries into the
 * configuration on one machine says nothing about another, and because a prompt
 * that comes back every connect is a prompt people learn to dismiss without
 * reading. `declined` is remembered as deliberately as `accepted`; Settings is
 * where either can be revisited.
 */
export type HostSetupDecision = "accepted" | "declined";

/**
 * What a *new* workspace on one host starts with.
 *
 * Per host profile, because a path is a statement about one machine's
 * filesystem and a startup command is a statement about one machine's shell.
 * A laptop's `~/dev` says nothing about a build box, and applying either
 * across hosts is how a create fails on a directory that only exists
 * somewhere else.
 *
 * Both fields are absent rather than empty when unset, and absent means "keep
 * doing what this app did before the setting existed".
 */
export interface WorkspaceDefaults {
  /** Where the first pane starts. Absent: wherever tmux would have started it. */
  directory?: string;
  /** Shell text sent to the first pane once, right after the create. Absent: nothing is sent. */
  startupCommand?: string;
}

/** The same ceiling `hostSetup` and `workspaceDefaults` use: one entry per host profile. */
export const MAX_RESTORE_HOSTS = MAX_WORKSPACE_DEFAULT_HOSTS;
/** Tabs one remembered workspace may hold; mirrors `MAX_RESTORE_TABS` in `app_state.rs`. */
export const MAX_RESTORE_TABS_PER_WORKSPACE = 64;
/** Workspaces one host may remember; mirrors `MAX_RESTORE_WORKSPACES` in `app_state.rs`. */
export const MAX_RESTORE_WORKSPACES_PER_HOST = 256;

/**
 * One remembered terminal inside a restored workspace, by the directory its
 * pane was in. The tab's name is deliberately not stored: restoring it would
 * take `-n`, and `-n` switches tmux's automatic window renaming off for that
 * tab forever, and the agents that name their tab after the task would lose it.
 */
export interface WorkspaceRestoreTab {
  /** Absent restores the tab wherever tmux would have started it. */
  directory?: string;
}

/**
 * One workspace the app last saw open, named the way tmux names it.
 *
 * Name, not tmux's `$N` id, because the case restore exists for is the tmux
 * server that died with a reboot, and its ids die with it. The name is the one
 * identity that survives the server; it is also what tells a restore that the
 * workspace is already open and must not be created a second time.
 */
export interface WorkspaceRestoreWorkspace {
  name: string;
  /** In tab-strip order. The first entry is the session's own first window. */
  tabs: WorkspaceRestoreTab[];
}

/** What one host profile had open the last time this app was watching it. */
export interface WorkspaceRestoreHost {
  workspaces: WorkspaceRestoreWorkspace[];
  /** The workspace that was on screen, by name for the same reason as above. */
  selectedSessionName?: string;
}

export interface PersistedAppState {
  schemaVersion: 1;
  appTabs: AppOwnedTab[];
  workspaceUi: WorkspaceUiRecord[];
  shell: ShellState;
  commands: { shortcutOverrides: Record<string, string | null> };
  hostSetup: Record<string, HostSetupDecision>;
  workspaceDefaults: Record<string, WorkspaceDefaults>;
  workspaceRestore: Record<string, WorkspaceRestoreHost>;
}

export const defaultShellState: ShellState = {
  panelSurface: "files",
  enabledPanelExtensions: ["automations"],
  sidebarCollapsed: false,
  sidebarWidth: SIDEBAR_MIN_WIDTH,
  panelOpen: false,
  panelWidth: 300,
  dockFraction: DOCK_DEFAULT_FRACTION,
  agentSort: "workspace",
  agentsSectionRatio: AGENTS_SECTION_DEFAULT_RATIO,
  agentStateGlyphs: false,
  compactWorkspaces: true,
  tabsInTitleBar: true,
  focusFollowsMouse: false,
  pinnedOnly: false,
  terminalScreenReader: false,
  copyOnSelect: true,
  cleanWrappedCommands: true,
  terminalApplicationClipboard: true,
  terminalFontSize: 13,
  theme: "system",
  uiScale: 1,
  markdownAppearance: "default",
  newWorkspaceHostProfileId: undefined,
  defaultMarkdownView: "split",
  restoreWorkspaces: false,
};

export const defaultAppState: PersistedAppState = {
  schemaVersion: 1,
  appTabs: [],
  workspaceUi: [],
  shell: defaultShellState,
  commands: { shortcutOverrides: {} },
  hostSetup: {},
  workspaceDefaults: {},
  workspaceRestore: {},
};

export function hostProfileId(connection: ConnectionSpec): string {
  return connection.mode === "local" ? "local" : connection.profileId;
}

export function normalizePersistedAppState(value: unknown): PersistedAppState {
  if (!value || typeof value !== "object") return defaultAppState;
  const candidate = value as Partial<PersistedAppState>;
  if (candidate.schemaVersion !== 1 || !Array.isArray(candidate.appTabs) || !Array.isArray(candidate.workspaceUi)) {
    return defaultAppState;
  }
  // Phase 11 replaced the shell preferences wholesale (three collapsible rails
  // became one sidebar plus one panel). The schema version stays at 1
  // deliberately: bumping it would throw away the user's open document tabs,
  // workspace selections and shortcut overrides to reset four booleans. Every
  // shell field is read defensively instead, so state written by the previous
  // build loads with the new preferences at their defaults.
  const shell = candidate.shell as Partial<ShellState> | undefined;
  const enabledPanelExtensions = Array.isArray(shell?.enabledPanelExtensions)
    ? [...new Set(shell.enabledPanelExtensions.filter((id): id is string => typeof id === "string" && id !== ""))]
    : [...defaultShellState.enabledPanelExtensions];
  return {
    schemaVersion: 1,
    appTabs: candidate.appTabs,
    workspaceUi: candidate.workspaceUi,
    shell: {
      // An extension turned off, or one this build does not have, is Files.
      panelSurface: typeof shell?.panelSurface === "string"
        && ((BUILT_IN_PANEL_SURFACES as readonly string[]).includes(shell.panelSurface)
          || enabledPanelExtensions.includes(shell.panelSurface))
        ? shell.panelSurface
        : "files",
      enabledPanelExtensions,
      sidebarCollapsed: Boolean(shell?.sidebarCollapsed),
      sidebarWidth: clampedSidebarWidth(shell?.sidebarWidth),
      panelOpen: Boolean(shell?.panelOpen),
      panelWidth: clampedPanelWidth(shell?.panelWidth),
      dockFraction: clampedDockFraction(shell?.dockFraction),
      agentSort: migratedAgentSort(shell?.agentSort),
      agentsSectionRatio: clampedAgentsRatio(shell?.agentsSectionRatio),
      agentStateGlyphs: Boolean(shell?.agentStateGlyphs),
      compactWorkspaces: typeof shell?.compactWorkspaces === "boolean"
        ? shell.compactWorkspaces
        : defaultShellState.compactWorkspaces,
      tabsInTitleBar: typeof shell?.tabsInTitleBar === "boolean"
        ? shell.tabsInTitleBar
        : defaultShellState.tabsInTitleBar,
      focusFollowsMouse: Boolean(shell?.focusFollowsMouse),
      pinnedOnly: Boolean(shell?.pinnedOnly),
      terminalScreenReader: Boolean(shell?.terminalScreenReader),
      copyOnSelect: typeof shell?.copyOnSelect === "boolean"
        ? shell.copyOnSelect
        : defaultShellState.copyOnSelect,
      cleanWrappedCommands: typeof shell?.cleanWrappedCommands === "boolean"
        ? shell.cleanWrappedCommands
        : defaultShellState.cleanWrappedCommands,
      terminalApplicationClipboard: typeof shell?.terminalApplicationClipboard === "boolean"
        ? shell.terminalApplicationClipboard
        : defaultShellState.terminalApplicationClipboard,
      terminalFontSize: clampedTerminalFontSize(shell?.terminalFontSize),
      theme: isThemePreference(shell?.theme) ? shell.theme : defaultShellState.theme,
      uiScale: UI_SCALES.find((scale) => scale === shell?.uiScale) ?? defaultShellState.uiScale,
      markdownAppearance: isMarkdownAppearance(shell?.markdownAppearance) ? shell.markdownAppearance : defaultShellState.markdownAppearance,
      newWorkspaceHostProfileId: typeof shell?.newWorkspaceHostProfileId === "string" && shell.newWorkspaceHostProfileId.trim()
        ? shell.newWorkspaceHostProfileId
        : undefined,
      defaultMarkdownView: normalizedViewMode(shell?.defaultMarkdownView),
      restoreWorkspaces: Boolean(shell?.restoreWorkspaces),
      ...(shell?.windowGeometry && validWindowGeometry(shell.windowGeometry)
        ? { windowGeometry: shell.windowGeometry } : {}),
    },
    commands: { shortcutOverrides: normalizeShortcutRecord(candidate.commands?.shortcutOverrides) },
    hostSetup: normalizeHostSetup(candidate.hostSetup),
    workspaceDefaults: normalizeWorkspaceDefaults(candidate.workspaceDefaults),
    workspaceRestore: normalizeWorkspaceRestore(candidate.workspaceRestore),
  };
}

/** An unrecognised mode is the default, not a tab that renders nothing. */
function normalizedViewMode(value: unknown): AppTabViewMode {
  return value === "source" || value === "live" || value === "preview" || value === "split" ? value : defaultShellState.defaultMarkdownView;
}

/**
 * The per-host workspace defaults, with everything unusable dropped.
 *
 * An empty or whitespace-only string is not a value here: it is what the field
 * looks like when the user cleared it, and storing it would make "unset" and
 * "set to nothing" two different states the create path would have to tell
 * apart. The entry cap matches `hostSetup` for the same reason — a map keyed
 * by host profile has no business being unbounded.
 */
function normalizeWorkspaceDefaults(value: unknown): Record<string, WorkspaceDefaults> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const entries: [string, WorkspaceDefaults][] = [];
  for (const [host, defaults] of Object.entries(value)) {
    if (entries.length >= MAX_WORKSPACE_DEFAULT_HOSTS) break;
    // An unusable entry is skipped, never a reason to stop reading: `""` is a
    // legal JSON key, and abandoning the loop on one would silently drop every
    // host after it.
    if (!host || !defaults || typeof defaults !== "object" || Array.isArray(defaults)) continue;
    const record = defaults as Partial<WorkspaceDefaults>;
    const directory = usableWorkspaceDefault(record.directory);
    const startupCommand = usableWorkspaceDefault(record.startupCommand);
    const entry: WorkspaceDefaults = {
      ...(directory ? { directory } : {}),
      ...(startupCommand ? { startupCommand } : {}),
    };
    if (entry.directory || entry.startupCommand) entries.push([host, entry]);
  }
  return Object.fromEntries(entries);
}

function usableDefault(value: unknown): value is string {
  return typeof value === "string" && value.trim() !== "";
}

/** One workspace default, trimmed and bounded, or `undefined` when there is none. */
export function usableWorkspaceDefault(value: unknown): string | undefined {
  if (!usableDefault(value)) return undefined;
  return value.trim().slice(0, MAX_WORKSPACE_DEFAULT_LENGTH);
}

/**
 * One restore record's text: bounded, and without the control characters that
 * would make the host refuse the create it describes. A file edited by hand
 * can carry them; a tmux session name or a pane path written by this app never
 * does, so dropping such an entry loses nothing real.
 */
function restorableText(value: unknown): string | undefined {
  if (typeof value !== "string" || value === "" || /[\u0000-\u001f\u007f]/.test(value)) return undefined;
  return value.slice(0, MAX_WORKSPACE_DEFAULT_LENGTH);
}

/**
 * The per-host workspace-restore records, with everything unusable dropped.
 *
 * An entry is kept when it can be *restored*, not when it merely parses: a
 * workspace without a usable name would create nothing, so the entry goes; a
 * tab without a directory stays, because "tmux's own choice" is a real state
 * and not a missing value. The caps mirror `app_state.rs`, which refuses the
 * whole save past them.
 */
function normalizeWorkspaceRestore(value: unknown): Record<string, WorkspaceRestoreHost> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const entries: [string, WorkspaceRestoreHost][] = [];
  for (const [host, raw] of Object.entries(value)) {
    if (entries.length >= MAX_RESTORE_HOSTS) break;
    if (!host || !raw || typeof raw !== "object" || Array.isArray(raw)) continue;
    const record = raw as Partial<WorkspaceRestoreHost>;
    const workspaces: WorkspaceRestoreWorkspace[] = [];
    for (const item of Array.isArray(record.workspaces) ? record.workspaces : []) {
      if (workspaces.length >= MAX_RESTORE_WORKSPACES_PER_HOST) break;
      if (!item || typeof item !== "object" || Array.isArray(item)) continue;
      const candidate = item as Partial<WorkspaceRestoreWorkspace>;
      const name = restorableText(candidate.name);
      if (!name || workspaces.some((seen) => seen.name === name)) continue;
      const tabs: WorkspaceRestoreTab[] = [];
      for (const tab of Array.isArray(candidate.tabs) ? candidate.tabs : []) {
        if (tabs.length >= MAX_RESTORE_TABS_PER_WORKSPACE) break;
        if (!tab || typeof tab !== "object" || Array.isArray(tab)) continue;
        const directory = restorableText((tab as WorkspaceRestoreTab).directory);
        tabs.push(directory ? { directory } : {});
      }
      workspaces.push({ name, tabs });
    }
    const selectedSessionName = restorableText(record.selectedSessionName);
    entries.push([host, { workspaces, ...(selectedSessionName ? { selectedSessionName } : {}) }]);
  }
  return Object.fromEntries(entries);
}

/**
 * The agent ordering, under whichever name the file was written with.
 *
 * `priority` and `grouped` are the previous names of the two orderings that
 * are now `status` and `workspace`; the sorts themselves did not change. A
 * saved value is therefore migrated rather than dropped — falling back to the
 * default here would silently put every user who had picked the other order
 * back on this one, for a change that was only ever about wording.
 */
function migratedAgentSort(value: unknown): AgentSortMode {
  if (value === "priority") return "status";
  if (value === "grouped") return "workspace";
  // `pinned` was added under its own name, so it needs no migration arm.
  return isAgentSortMode(value) ? value : defaultShellState.agentSort;
}

/**
 * An unrecognised decision is dropped rather than coerced. Coercing it to
 * `declined` would silently suppress the prompt on a host the user never
 * answered for, and coercing it to `accepted` would be worse.
 */
function normalizeHostSetup(value: unknown): Record<string, HostSetupDecision> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(Object.entries(value)
    .filter(([host, decision]) => host && (decision === "accepted" || decision === "declined"))) as Record<string, HostSetupDecision>;
}

/** Never narrower than the token width; the window cap is applied at render. */
export function clampedSidebarWidth(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > SIDEBAR_MIN_WIDTH
    ? Math.min(Math.round(value), 4_000)
    : SIDEBAR_MIN_WIDTH;
}

/**
 * The panel's stored width, never narrower than the token minimum; the window
 * cap is applied at render, as it is for the sidebar.
 *
 * A save with no width at all falls back to the default (300px) rather than to
 * the minimum: unlike the sidebar, whose default *is* its minimum, a panel
 * reset to 240px here would silently narrow every panel written by a build
 * before this field existed.
 */
export function clampedPanelWidth(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return defaultShellState.panelWidth;
  return Math.min(Math.max(Math.round(value), PANEL_MIN_WIDTH), 4_000);
}

export function clampedDockFraction(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return DOCK_DEFAULT_FRACTION;
  return Math.min(DOCK_MAX_FRACTION, Math.max(DOCK_MIN_FRACTION, value));
}

export function clampedAgentsRatio(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return AGENTS_SECTION_DEFAULT_RATIO;
  return Math.min(AGENTS_SECTION_MAX_RATIO, Math.max(AGENTS_SECTION_MIN_RATIO, value));
}

export function clampedTerminalFontSize(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return defaultShellState.terminalFontSize;
  return Math.min(TERMINAL_FONT_SIZE_MAX, Math.max(TERMINAL_FONT_SIZE_MIN, Math.round(value)));
}

/** The sidebar may never eat more than a third of the window (cmux rule). */
export function sidebarWidthForWindow(width: number, windowWidth: number): number {
  const cap = Math.max(SIDEBAR_MIN_WIDTH, Math.floor(windowWidth * SIDEBAR_MAX_WINDOW_FRACTION));
  return Math.min(clampedSidebarWidth(width), cap);
}

/** The panel may never eat more than half the window; the sidebar's rule, from the other edge. */
export function panelWidthForWindow(width: number, windowWidth: number): number {
  const cap = Math.max(PANEL_MIN_WIDTH, Math.floor(windowWidth * PANEL_MAX_WINDOW_FRACTION));
  return Math.min(clampedPanelWidth(width), cap);
}

function validWindowGeometry(value: unknown): value is NonNullable<PersistedAppState["shell"]["windowGeometry"]> {
  if (!value || typeof value !== "object") return false;
  const item = value as Record<string, unknown>;
  return [item.x, item.y, item.width, item.height].every((number) => typeof number === "number" && Number.isFinite(number))
    && (item.width as number) >= 320 && (item.height as number) >= 240 && typeof item.maximized === "boolean"
    && (item.scaleFactorMilli === undefined || (
      typeof item.scaleFactorMilli === "number" && Number.isInteger(item.scaleFactorMilli)
      && item.scaleFactorMilli >= 500 && item.scaleFactorMilli <= 8_000
    ));
}

function normalizeShortcutRecord(value: unknown): Record<string, string | null> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(Object.entries(value).filter(([, binding]) => typeof binding === "string" || binding === null));
}
