import type { ConnectionSpec } from "../../app/types";
import { isAgentSortMode, type AgentSortMode } from "../agents/agentsList";

export type AppTabKind = "file" | "markdown" | "gitDiff";
/** The two halves of the right panel; they share one 300px surface. */
export type PanelSurface = "files" | "git";

/** Sidebar geometry, from the plan's token table (cmux). */
export const SIDEBAR_MIN_WIDTH = 240;
/** Fraction of the window a dragged sidebar may not exceed. */
export const SIDEBAR_MAX_WINDOW_FRACTION = 1 / 3;
/** Right panel geometry; the same rules as the sidebar, from its own edge. */
export const PANEL_MIN_WIDTH = 240;
/** Fraction of the window a dragged panel may not exceed. */
export const PANEL_MAX_WINDOW_FRACTION = 1 / 2;
/** Share of the sidebar's height the agents section takes by default. */
export const AGENTS_SECTION_DEFAULT_RATIO = 0.42;
export const AGENTS_SECTION_MIN_RATIO = 0.15;
export const AGENTS_SECTION_MAX_RATIO = 0.75;

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
  viewMode?: "source" | "preview" | "split";
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
}

export interface ShellState {
  /** Which half of the right panel is showing when it is open. */
  panelSurface: PanelSurface;
  /** ⌘B. Collapsed means 0px, not a 44px icon strip. */
  sidebarCollapsed: boolean;
  /** Drag-resizable; clamped against the window at render time. */
  sidebarWidth: number;
  /** ⌥⌘B. Closed by default, and it reserves no space when closed. */
  panelOpen: boolean;
  /** Drag-resizable from its left edge; clamped against the window at render time. */
  panelWidth: number;
  /** The agents section's one control. */
  agentSort: AgentSortMode;
  /** Position of the sidebar's internal divider, as a share of its height. */
  agentsSectionRatio: number;
  /** Draws a shape inside each state dot as well as coloring it. */
  agentStateGlyphs: boolean;
  /**
   * Makes terminal *content* readable to a screen reader. Off by default
   * because xterm's screen-reader mode costs a string allocation and an
   * emitter dispatch per printed codepoint (P12-U002); this is the setting
   * that Phase 12 deferred to Phase 11's settings surface.
   */
  terminalScreenReader: boolean;
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

export interface PersistedAppState {
  schemaVersion: 1;
  appTabs: AppOwnedTab[];
  workspaceUi: WorkspaceUiRecord[];
  shell: ShellState;
  commands: { shortcutOverrides: Record<string, string | null> };
  hostSetup: Record<string, HostSetupDecision>;
}

export const defaultShellState: ShellState = {
  panelSurface: "files",
  sidebarCollapsed: false,
  sidebarWidth: SIDEBAR_MIN_WIDTH,
  panelOpen: false,
  panelWidth: 300,
  agentSort: "workspace",
  agentsSectionRatio: AGENTS_SECTION_DEFAULT_RATIO,
  agentStateGlyphs: false,
  terminalScreenReader: false,
};

export const defaultAppState: PersistedAppState = {
  schemaVersion: 1,
  appTabs: [],
  workspaceUi: [],
  shell: defaultShellState,
  commands: { shortcutOverrides: {} },
  hostSetup: {},
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
  return {
    schemaVersion: 1,
    appTabs: candidate.appTabs,
    workspaceUi: candidate.workspaceUi,
    shell: {
      panelSurface: shell?.panelSurface === "git" ? "git" : "files",
      sidebarCollapsed: Boolean(shell?.sidebarCollapsed),
      sidebarWidth: clampedSidebarWidth(shell?.sidebarWidth),
      panelOpen: Boolean(shell?.panelOpen),
      panelWidth: clampedPanelWidth(shell?.panelWidth),
      agentSort: migratedAgentSort(shell?.agentSort),
      agentsSectionRatio: clampedAgentsRatio(shell?.agentsSectionRatio),
      agentStateGlyphs: Boolean(shell?.agentStateGlyphs),
      terminalScreenReader: Boolean(shell?.terminalScreenReader),
      ...(shell?.windowGeometry && validWindowGeometry(shell.windowGeometry)
        ? { windowGeometry: shell.windowGeometry } : {}),
    },
    commands: { shortcutOverrides: normalizeShortcutRecord(candidate.commands?.shortcutOverrides) },
    hostSetup: normalizeHostSetup(candidate.hostSetup),
  };
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

export function clampedAgentsRatio(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return AGENTS_SECTION_DEFAULT_RATIO;
  return Math.min(AGENTS_SECTION_MAX_RATIO, Math.max(AGENTS_SECTION_MIN_RATIO, value));
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
