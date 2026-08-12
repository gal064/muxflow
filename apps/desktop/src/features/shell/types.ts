import type { ConnectionSpec } from "../../app/types";

export type AppTabKind = "file" | "markdown" | "gitDiff";
export type ExplorerSurface = "explorer" | "git";

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

export interface PersistedAppState {
  schemaVersion: 1;
  appTabs: AppOwnedTab[];
  workspaceUi: WorkspaceUiRecord[];
  shell: {
    explorerSurface: ExplorerSurface;
    explorerCollapsed: boolean;
    agentSidebarCollapsed: boolean;
    /** Physical geometry plus the capture scale, used to preserve logical size across monitors. */
    windowGeometry?: { x: number; y: number; width: number; height: number; maximized: boolean; scaleFactorMilli?: number };
  };
  commands: { shortcutOverrides: Record<string, string | null> };
}

export const defaultAppState: PersistedAppState = {
  schemaVersion: 1,
  appTabs: [],
  workspaceUi: [],
  shell: {
    explorerSurface: "explorer",
    explorerCollapsed: false,
    agentSidebarCollapsed: false,
  },
  commands: { shortcutOverrides: {} },
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
  const surface = candidate.shell?.explorerSurface === "git" ? "git" : "explorer";
  return {
    schemaVersion: 1,
    appTabs: candidate.appTabs,
    workspaceUi: candidate.workspaceUi,
    shell: {
      explorerSurface: surface,
      explorerCollapsed: Boolean(candidate.shell?.explorerCollapsed),
      agentSidebarCollapsed: Boolean(candidate.shell?.agentSidebarCollapsed),
      ...(candidate.shell?.windowGeometry && validWindowGeometry(candidate.shell.windowGeometry)
        ? { windowGeometry: candidate.shell.windowGeometry } : {}),
    },
    commands: { shortcutOverrides: normalizeShortcutRecord(candidate.commands?.shortcutOverrides) },
  };
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
