export interface Session {
  id: string;
  name: string;
  windowCount: number;
  attachedClients: number;
  order?: number;
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
}

export interface PersistedProfiles {
  schemaVersion: number;
  profiles: HostProfile[];
  lastProfileId?: string;
  recovery?: { preservedPath: string; error: string } | null;
}
