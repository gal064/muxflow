export interface Session {
  id: string;
  name: string;
  windowCount: number;
  attachedClients: number;
  order?: number;
  /**
   * Pinned to the top of the workspace list, from the host's own sidecar.
   *
   * Not a tmux property and not app state: the host overlays it onto every
   * snapshot, so every client of one tmux server agrees about what is pinned.
   */
  pinned?: boolean;
}

export interface Window {
  id: string;
  sessionId: string;
  index: number;
  name: string;
  active: boolean;
  layout: string;
  zoomed?: boolean;
  layoutGeneration?: number;
  /** Pinned to the front of its workspace's strip; see {@link Session.pinned}. */
  pinned?: boolean;
}

export interface Pane {
  id: string;
  sessionId: string;
  windowId: string;
  index: number;
  active: boolean;
  width: number;
  height: number;
  left: number;
  top: number;
  currentPath: string;
  currentCommand: string;
}

export interface TmuxSnapshot {
  sessions: Session[];
  windows: Window[];
  panes: Pane[];
}

export type ConnectionSpec =
  | { mode: "local" }
  | { mode: "ssh"; profileId: string; target: string; configPath?: string };

export interface HostProfile {
  id: string;
  label: string;
  connection: ConnectionSpec;
  /** One character. Absent means "derive from the label". */
  letter?: string;
  /** Checked in the host chooser. Absent means false. */
  shown?: boolean;
}

export interface PersistedProfiles {
  schemaVersion: number;
  profiles: HostProfile[];
  lastProfileId?: string;
  recovery?: { preservedPath: string; error: string } | null;
}
