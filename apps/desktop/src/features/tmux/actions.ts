import { invoke } from "@tauri-apps/api/core";
import { measureHostRoundTrip } from "../shell/hostLatency";
import { measurePerfRequest, recordPerfCounter } from "../../perf/probe";


export type TmuxActionKind =
  | "createSession" | "renameSession" | "reorderSession" | "selectSession" | "closeSession"
  | "createWindow" | "renameWindow" | "reorderWindow" | "selectWindow" | "closeWindow"
  | "splitPaneRight" | "splitPaneDown" | "focusPane"
  | "resizePaneLeft" | "resizePaneRight" | "resizePaneUp" | "resizePaneDown"
  | "zoomPane" | "closePane"
  | "setPinned";

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
  /**
   * What `setPinned` writes for its session, or for `windowId` inside it when
   * that is given. A pin is host state rather than a tmux mutation, so it takes
   * this pipeline for the scope checks and the authoritative snapshot that
   * follows, not because tmux is sent anything.
   */
  pinned?: boolean;
  confirmed?: boolean;
  /**
   * Where a created session's first pane starts (`createSession` only).
   *
   * Resolved and validated on the host that owns the filesystem, not here: the
   * path may name a directory only the remote machine has, and a create that
   * half-succeeds is worse than one that is refused.
   */
  directory?: string;
}

export interface AuthoritativePrecondition {
  serverIdentity: string;
  generation: number;
}

/**
 * The native half of one action's switch timeline, passed straight through to
 * the `perf.timeline` record. Present only when the process is running a
 * measured build with `ADE_PERF_LOG` set; see `perf_log/switch_timing.rs` for
 * what each stamp means. Deliberately opaque here: this layer joins it, it does
 * not interpret it.
 */
export type TmuxActionTiming = Record<string, unknown>;

export interface TmuxActionResult {
  sessionId?: string;
  windowId?: string;
  paneId?: string;
  topologyGeneration: number;
  timing?: TmuxActionTiming;
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
  pinned: boolean;
  expected_server_identity: string;
  expected_generation: number;
  confirmed: boolean;
  directory: string;
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
    pinned: action.pinned ?? false,
    expected_server_identity: precondition.serverIdentity,
    expected_generation: precondition.generation,
    confirmed: action.confirmed ?? false,
    directory: action.directory ?? "",
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
  const wire = toWireTmuxAction(action, precondition);
  const boundary = { clientId, action: wire };
  recordPerfCounter(`tmux.action.${action.kind}.requests`);
  return measurePerfRequest(`tmux.action.${action.kind}`, "tmux", boundary, async (request) => {
    const result = await measureHostRoundTrip(invoke<TmuxActionResult>("tmux_action", request));
    if (!result || !Number.isSafeInteger(result.topologyGeneration) || result.topologyGeneration < 0) {
      throw new Error("Native tmux action returned invalid topology metadata.");
    }
    return result;
  }, { byteCounters: ["tmux.action.requestBytes"] });
}

export function isDestructiveTmuxAction(action: TmuxAction): boolean {
  return action.kind === "closeSession" || action.kind === "closeWindow" || action.kind === "closePane";
}

export function isStaleTmuxTopologyError(error: unknown): boolean {
  return /stale topology|generation changed/i.test(String(error ?? ""));
}
