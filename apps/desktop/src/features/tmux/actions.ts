import { invoke } from "@tauri-apps/api/core";
import { measureHostRoundTrip } from "../shell/hostLatency";

export type TmuxActionKind =
  | "createSession" | "renameSession" | "reorderSession" | "selectSession" | "closeSession"
  | "createWindow" | "renameWindow" | "reorderWindow" | "selectWindow" | "closeWindow"
  | "splitPaneRight" | "splitPaneDown" | "focusPane"
  | "resizePaneLeft" | "resizePaneRight" | "resizePaneUp" | "resizePaneDown"
  | "zoomPane" | "closePane";

export interface TmuxAction {
  kind: TmuxActionKind;
  sessionId?: string;
  windowId?: string;
  paneId?: string;
  name?: string;
  index?: number;
  targetWindowId?: string;
  relativePosition?: "before" | "after";
  splitSize?: number;
  resizeCells?: number;
  zoomed?: boolean;
  confirmed?: boolean;
}

export interface AuthoritativePrecondition {
  serverIdentity: string;
  generation: number;
}

export interface TmuxActionResult {
  sessionId?: string;
  windowId?: string;
  paneId?: string;
  topologyGeneration: number;
}

interface WireTmuxAction {
  kind: TmuxActionKind;
  session_id: string;
  window_id: string;
  pane_id: string;
  name: string;
  index: number;
  target_window_id: string;
  relative_position: "before" | "after" | "unspecified";
  split_size: number;
  resize_cells: number;
  zoomed: boolean;
  expected_server_identity: string;
  expected_generation: number;
  confirmed: boolean;
}

export function toWireTmuxAction(
  action: TmuxAction,
  precondition: AuthoritativePrecondition,
): WireTmuxAction {
  if (action.kind === "reorderWindow" && (
    !action.targetWindowId
    || action.targetWindowId === action.windowId
    || (action.relativePosition !== "before" && action.relativePosition !== "after")
  )) {
    throw new Error("window reorder requires a distinct target window ID and before/after position");
  }
  return {
    kind: action.kind,
    session_id: action.sessionId ?? "",
    window_id: action.windowId ?? "",
    pane_id: action.paneId ?? "",
    name: action.name ?? "",
    index: action.index ?? 0,
    target_window_id: action.targetWindowId ?? "",
    relative_position: action.relativePosition ?? "unspecified",
    split_size: action.splitSize ?? 0,
    resize_cells: action.resizeCells ?? 0,
    zoomed: action.zoomed ?? false,
    expected_server_identity: precondition.serverIdentity,
    expected_generation: precondition.generation,
    confirmed: action.confirmed ?? false,
  };
}

export function requestTmuxAction(
  clientId: string,
  action: TmuxAction,
  precondition: AuthoritativePrecondition,
): Promise<TmuxActionResult> {
  // Timed on the way past: this is a real host round-trip the app was making
  // anyway, which is where the sidebar's latency readout comes from without
  // adding a single request of its own.
  return measureHostRoundTrip(invoke<TmuxActionResult>("tmux_action", {
    clientId,
    action: toWireTmuxAction(action, precondition),
  }));
}

export function isDestructiveTmuxAction(action: TmuxAction): boolean {
  return action.kind === "closeSession" || action.kind === "closeWindow" || action.kind === "closePane";
}

export function isStaleTmuxTopologyError(error: unknown): boolean {
  return /stale topology|generation changed/i.test(String(error ?? ""));
}
