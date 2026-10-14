import type { Pane, Session, Window as TmuxWindow } from "../../app/types";
import type { AppOwnedTab } from "../shell/types";
import type { TmuxAction } from "../tmux/actions";
import type { DragPayload, DropTarget } from "./pointerDrag";

/** The active host's topology: every move stays on one tmux server. */
export interface DropContext {
  hostProfileId: string;
  sessions: readonly Session[];
  windows: readonly TmuxWindow[];
  panes: readonly Pane[];
  /** This server's document tabs, so a move never opens one twice in a workspace. */
  appTabs: readonly Pick<AppOwnedTab, "id" | "sessionId" | "kind" | "resource">[];
}

export type DropPlan =
  /**
   * A join needs no follow-up focus: the host leaves the window's active pane
   * alone, unless the pane it moves is that active pane, which keeps it.
   */
  | { kind: "action"; action: TmuxAction }
  | { kind: "moveAppTab"; tabId: string; targetTabId: string; side: "before" | "after" }
  | { kind: "moveAppTabToWorkspace"; tabId: string; session: Session }
  | { kind: "refuse"; reason: string };

export const CLOSES_WORKSPACE = "Moving a workspace's only tab would close the workspace.";
export const ONE_PANE_TAB_ONLY = "Only a tab with a single pane can become a split.";
export const NO_DOCUMENT_SPLITS = "File tabs can't become splits.";
export const SAME_HOST_ONLY = "Tabs and agents can only move within one host.";
export const ALREADY_OPEN = "That file is already open in the workspace.";

/**
 * What releasing `payload` on `target` would do, or `undefined` when it would
 * do nothing (dropped on itself, or onto a group it does not belong to).
 *
 * Pure, and the same answer the host would give: a move that would empty the
 * workspace it leaves is refused here so the drag never draws a marker for it,
 * and refused again on the host, which is where the guard actually holds.
 */
export function planDrop(payload: DragPayload, dropped: DropTarget, context: DropContext): DropPlan | undefined {
  if (payload.hostProfileId !== context.hostProfileId) return { kind: "refuse", reason: SAME_HOST_ONLY };
  // A pane dropped on a tab is dropped on the strip: it becomes a tab of the
  // strip's workspace, appended, the same move as dropping it on that row.
  const target: DropTarget = payload.kind === "pane" && dropped.kind === "tab" && dropped.workspace
    ? { kind: "workspace", ...dropped.workspace }
    : dropped;
  if (target.kind === "workspace" && target.hostProfileId !== context.hostProfileId) return { kind: "refuse", reason: SAME_HOST_ONLY };
  const windowsIn = (sessionId: string) => context.windows.filter((window) => window.sessionId === sessionId).length;
  const panesIn = (windowId: string) => context.panes.filter((pane) => pane.windowId === windowId);
  const session = (sessionId: string) => context.sessions.find((item) => item.id === sessionId);
  const targetPane = target.kind === "pane" ? context.panes.find((pane) => pane.id === target.paneId) : undefined;

  switch (payload.kind) {
    case "terminalTab": {
      const window = context.windows.find((item) => item.id === payload.windowId);
      if (!window) return undefined;
      if (target.kind === "tab") {
        const targetWindowId = terminalTabWindowId(target.tabKey);
        if (!targetWindowId || targetWindowId === window.id) return undefined;
        const targetWindow = context.windows.find((item) => item.id === targetWindowId);
        if (targetWindow?.sessionId !== window.sessionId) return undefined;
        // Pinned tabs are drawn first whatever their window order, so a drop
        // across that boundary would land somewhere other than its marker.
        if (Boolean(targetWindow.pinned) !== Boolean(window.pinned)) return undefined;
        return {
          kind: "action",
          action: { kind: "reorderWindow", sessionId: window.sessionId, windowId: window.id, targetWindowId, relativePosition: target.side },
        };
      }
      if (target.kind === "workspace") {
        if (target.sessionId === window.sessionId || !session(target.sessionId)) return undefined;
        if (windowsIn(window.sessionId) === 1) return { kind: "refuse", reason: CLOSES_WORKSPACE };
        return { kind: "action", action: { kind: "moveWindow", sessionId: window.sessionId, windowId: window.id, targetSessionId: target.sessionId } };
      }
      if (!targetPane || targetPane.windowId === window.id) return undefined;
      const panes = panesIn(window.id);
      if (panes.length !== 1) return { kind: "refuse", reason: ONE_PANE_TAB_ONLY };
      if (windowsIn(window.sessionId) === 1) return { kind: "refuse", reason: CLOSES_WORKSPACE };
      return joinPlan(panes[0].id, targetPane.id, target);
    }
    case "appTab": {
      if (target.kind === "tab") {
        const targetTabId = appTabId(target.tabKey);
        if (!targetTabId || targetTabId === payload.tabId) return undefined;
        return { kind: "moveAppTab", tabId: payload.tabId, targetTabId, side: target.side };
      }
      if (target.kind === "workspace") {
        const destination = session(target.sessionId);
        if (!destination || destination.id === payload.sessionId) return undefined;
        const moving = context.appTabs.find((tab) => tab.id === payload.tabId);
        if (moving && context.appTabs.some((tab) => tab.sessionId === destination.id
          && tab.kind === moving.kind && tab.resource === moving.resource)) {
          return { kind: "refuse", reason: ALREADY_OPEN };
        }
        return { kind: "moveAppTabToWorkspace", tabId: payload.tabId, session: destination };
      }
      return targetPane ? { kind: "refuse", reason: NO_DOCUMENT_SPLITS } : undefined;
    }
    case "pane": {
      // The record can trail the topology; where the pane is *now* decides.
      const pane = context.panes.find((item) => item.id === payload.paneId);
      if (!pane) return undefined;
      const alone = panesIn(pane.windowId).length === 1;
      const emptiesWorkspace = alone && windowsIn(pane.sessionId) === 1;
      if (target.kind === "pane") {
        if (!targetPane || targetPane.id === pane.id) return undefined;
        // Inside its own window a join rearranges the panes and empties nothing.
        if (targetPane.windowId !== pane.windowId && emptiesWorkspace) return { kind: "refuse", reason: CLOSES_WORKSPACE };
        return joinPlan(pane.id, targetPane.id, target);
      }
      if (target.kind === "workspace") {
        if (!session(target.sessionId)) return undefined;
        // Alone in its window, the agent already is a tab: move that tab.
        if (alone) {
          if (target.sessionId === pane.sessionId) return undefined;
          if (emptiesWorkspace) return { kind: "refuse", reason: CLOSES_WORKSPACE };
          return { kind: "action", action: { kind: "moveWindow", sessionId: pane.sessionId, windowId: pane.windowId, targetSessionId: target.sessionId } };
        }
        // Sharing a window, it gets a tab of its own there — in its own
        // workspace too, which is how a split becomes a tab.
        return { kind: "action", action: { kind: "breakPane", paneId: pane.id, targetSessionId: target.sessionId } };
      }
      return undefined;
    }
  }
}

function joinPlan(paneId: string, targetPaneId: string, target: DropTarget): DropPlan | undefined {
  if (target.kind !== "pane") return undefined;
  return { kind: "action", action: { kind: "joinPane", paneId, targetPaneId, paneEdge: target.edge } };
}

function terminalTabWindowId(tabKey: string): string | undefined {
  return tabKey.startsWith("terminal:") ? tabKey.slice("terminal:".length) : undefined;
}

function appTabId(tabKey: string): string | undefined {
  return tabKey.startsWith("app:") ? tabKey.slice("app:".length) : undefined;
}
