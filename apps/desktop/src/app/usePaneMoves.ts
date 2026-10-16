import { useCallback, useEffect, useRef, type Dispatch, type SetStateAction } from "react";
import type { Pane, Session, TmuxSnapshot } from "./types";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { AgentListRow } from "../features/agents/agentsList";
import type { SelectableTab } from "../features/shell/model";
import { dockAppTab, moveAppTabTo, moveAppTabToWorkspace, orderedSessions, undockAppTab } from "../features/shell/model";
import type { DockSide, PersistedAppState } from "../features/shell/types";
import type { AgentMoves } from "../features/workspaces/WorkspaceSidebar";
import { stripAgentStatusGlyphs } from "../features/agents/agentLabels";
import { planDrop, type DropContext, type DropPlan } from "../features/dragDrop/dropRules";
import { setDropResolver, type DragPayload, type DropOutcome, type DropTarget } from "../features/dragDrop/pointerDrag";

interface PaneMovesOptions {
  hostProfileId: string;
  serverIdentity?: string;
  canMutate: boolean;
  snapshot: TmuxSnapshot;
  activeSession?: Session;
  activePane?: Pane;
  /** False while a document tab covers the terminal: a split made there would happen out of sight. */
  terminalOnScreen: boolean;
  appTabs: PersistedAppState["appTabs"];
  /** The document docked beside the terminal in the workspace on screen. */
  dock?: { tabId: string; side: DockSide };
  performAction(action: TmuxAction): Promise<TmuxActionResult | undefined>;
  /** Shows a pane wherever it now is: how focus follows the pane on screen when it moves. */
  selectPane(pane: Pane): void;
  /** Puts the keyboard back in a pane's terminal. */
  focusTerminal(paneId: string): void;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  setStatus(text: string): void;
}

/**
 * Moving tabs and agents between workspaces and splits, by drag or from an
 * agent's menu. Both ask `planDrop` against the host on screen, so a menu
 * item and a drop onto the same place always mean the same move.
 *
 * Every callback reads the latest options through a ref: a drag outlives many
 * snapshots, and the move it ends in must be planned against the last one.
 */
export function usePaneMoves(options: PaneMovesOptions) {
  const latest = useRef(options);
  latest.current = options;

  const context = (): DropContext => ({
    hostProfileId: latest.current.hostProfileId,
    sessions: latest.current.snapshot.sessions,
    windows: latest.current.snapshot.windows,
    panes: latest.current.snapshot.panes,
    appTabs: latest.current.appTabs.filter((tab) => tab.hostProfileId === latest.current.hostProfileId
      && tab.serverIdentity === latest.current.serverIdentity),
    dock: latest.current.dock,
  });

  /**
   * `fromMenu`: a menu hands focus back to the row it was opened from, so
   * after a move that leaves the view alone the keyboard goes back to the
   * pane on screen, where it was before the right-click. A drag restores
   * focus itself.
   */
  const run = useCallback((plan: DropPlan, fromMenu = false) => {
    const current = latest.current;
    switch (plan.kind) {
      case "action": {
        // Focus stays on what is on screen. A move of anything else leaves it
        // alone (a join never takes focus, and tmux re-picks only the source
        // window's or workspace's view), but a move of the pane or tab on
        // screen takes it along, to wherever the host says it landed. A
        // document covering the terminal is what is on screen, and stays.
        const followed = current.terminalOnScreen ? paneOnScreenMovedBy(plan.action, current.activePane) : undefined;
        const fromSessionId = current.activeSession?.id;
        void current.performAction(plan.action).then((result) => {
          // Somewhere else by the time it lands: the user has moved on.
          if (latest.current.activeSession?.id !== fromSessionId) return;
          if (followed && result?.sessionId && result.windowId) {
            latest.current.selectPane({ ...followed, sessionId: result.sessionId, windowId: result.windowId });
          } else if (fromMenu && latest.current.terminalOnScreen && latest.current.activePane) {
            latest.current.focusTerminal(latest.current.activePane.id);
          }
        });
        return;
      }
      case "moveAppTab":
        if (current.activeSession) {
          const session = current.activeSession;
          current.setAppState((state) => moveAppTabTo(
            state, current.hostProfileId, current.serverIdentity, session, plan.tabId, plan.targetTabId, plan.side,
          ));
        }
        return;
      case "dockAppTab":
      case "undockAppTab":
        if (current.activeSession && current.serverIdentity) {
          const session = current.activeSession;
          const serverIdentity = current.serverIdentity;
          current.setAppState((state) => {
            if (plan.kind === "dockAppTab") {
              return dockAppTab(state, current.hostProfileId, serverIdentity, session, plan.tabId, plan.side);
            }
            const undocked = undockAppTab(state, current.hostProfileId, serverIdentity, session);
            return plan.place
              ? moveAppTabTo(undocked, current.hostProfileId, serverIdentity, session, plan.tabId, plan.place.targetTabId, plan.place.side)
              : undocked;
          });
        }
        return;
      case "moveAppTabToWorkspace":
        current.setAppState((state) => moveAppTabToWorkspace(
          state, current.hostProfileId, current.serverIdentity, plan.tabId, plan.session,
        ));
        return;
      case "refuse":
        return;
    }
  }, []);

  const outcome = useCallback((payload: DragPayload, target: DropTarget): DropOutcome | undefined => {
    const plan = planDrop(payload, target, context());
    if (!plan) return undefined;
    if (plan.kind === "refuse") return { kind: "refuse", reason: plan.reason };
    return { kind: "run", run: () => run(plan) };
  }, [run]);

  useEffect(() => {
    setDropResolver(outcome, (reason) => latest.current.setStatus(reason));
    return () => setDropResolver(undefined);
  }, [outcome]);

  /** A terminal tab moves the host's window, so it drags only while the host can be changed. */
  const tabDragPayload = useCallback((tab: SelectableTab): DragPayload | undefined => {
    const { activeSession, canMutate, hostProfileId } = latest.current;
    if (!activeSession) return undefined;
    if (tab.kind === "terminal") {
      return canMutate
        ? { kind: "terminalTab", hostProfileId, sessionId: activeSession.id, windowId: tab.id, label: tab.title }
        : undefined;
    }
    return { kind: "appTab", hostProfileId, sessionId: activeSession.id, tabId: tab.id, label: tab.title };
  }, []);

  /** Only the host on screen: every move is planned against its topology. */
  const agentDragPayload = useCallback((row: AgentListRow): DragPayload | undefined => {
    const { canMutate, hostProfileId } = latest.current;
    const agent = row.agent;
    if (!canMutate || !row.routable || !agent.paneId || agent.hostProfileId !== hostProfileId) return undefined;
    return {
      kind: "pane", hostProfileId, sessionId: agent.sessionId, windowId: agent.windowId, paneId: agent.paneId, label: agent.displayName,
    };
  }, []);

  /** A pane grabbed with the link modifier: the same moves as its agent row, from the pane itself. */
  const paneDragPayload = useCallback((pane: Pane): DragPayload | undefined => {
    const { canMutate, hostProfileId, snapshot } = latest.current;
    if (!canMutate) return undefined;
    const window = snapshot.windows.find((item) => item.id === pane.windowId);
    return {
      kind: "pane", hostProfileId, sessionId: pane.sessionId, windowId: pane.windowId, paneId: pane.id,
      label: window ? stripAgentStatusGlyphs(window.name) : pane.id,
    };
  }, []);

  const agentMoves = useCallback((row: AgentListRow): AgentMoves | undefined => {
    const payload = agentDragPayload(row);
    if (payload?.kind !== "pane") return undefined;
    const { activePane, hostProfileId, snapshot, terminalOnScreen } = latest.current;
    const actionFor = (target: DropTarget) => {
      const plan = planDrop(payload, target, context());
      return plan?.kind === "action" ? () => run(plan, true) : undefined;
    };
    const pane = snapshot.panes.find((item) => item.id === payload.paneId);
    const workspaces = orderedSessions(snapshot.sessions)
      .flatMap((session) => {
        const move = actionFor({ kind: "workspace", hostProfileId, sessionId: session.id });
        // Its own workspace is offered only to split it out into a tab there.
        const name = session.id === pane?.sessionId ? `${session.name} (as its own tab)` : session.name;
        return move ? [{ id: session.id, name, run: move }] : [];
      });
    return {
      // From another tab only: an agent already beside you is already here.
      bringHere: activePane && terminalOnScreen && pane?.windowId !== activePane.windowId
        ? actionFor({ kind: "pane", paneId: activePane.id, edge: "right" })
        : undefined,
      workspaces,
    };
  }, [agentDragPayload, run]);

  return { tabDragPayload, agentDragPayload, paneDragPayload, agentMoves };
}

/**
 * The pane on screen when `action` carries it somewhere else: its window
 * moved to another workspace, or it broke out into a tab of its own. A join
 * is not here: the host keeps focus on the pane it had inside the window.
 */
export function paneOnScreenMovedBy(action: TmuxAction, activePane: Pane | undefined): Pane | undefined {
  if (!activePane) return undefined;
  if (action.kind === "moveWindow" && action.windowId === activePane.windowId) return activePane;
  if (action.kind === "breakPane" && action.paneId === activePane.id) return activePane;
  return undefined;
}
