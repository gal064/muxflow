import { useCallback, useEffect, useRef, type Dispatch, type SetStateAction } from "react";
import type { Pane, Session, TmuxSnapshot } from "./types";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { AgentListRow } from "../features/agents/agentsList";
import type { SelectableTab } from "../features/shell/model";
import { moveAppTabTo, moveAppTabToWorkspace, orderedSessions } from "../features/shell/model";
import type { PersistedAppState } from "../features/shell/types";
import type { AgentMoves } from "../features/workspaces/WorkspaceSidebar";
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
  performAction(action: TmuxAction): Promise<TmuxActionResult | undefined>;
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
  });

  const run = useCallback((plan: DropPlan) => {
    const current = latest.current;
    switch (plan.kind) {
      case "action":
        void current.performAction(plan.action);
        return;
      case "moveAppTab":
        if (current.activeSession) {
          const session = current.activeSession;
          current.setAppState((state) => moveAppTabTo(
            state, current.hostProfileId, current.serverIdentity, session, plan.tabId, plan.targetTabId, plan.side,
          ));
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
      kind: "agent", hostProfileId, sessionId: agent.sessionId, windowId: agent.windowId, paneId: agent.paneId, label: agent.displayName,
    };
  }, []);

  const agentMoves = useCallback((row: AgentListRow): AgentMoves | undefined => {
    const payload = agentDragPayload(row);
    if (payload?.kind !== "agent") return undefined;
    const { activePane, hostProfileId, snapshot, terminalOnScreen } = latest.current;
    const actionFor = (target: DropTarget) => {
      const plan = planDrop(payload, target, context());
      return plan?.kind === "action" ? () => run(plan) : undefined;
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
      bringHere: activePane && terminalOnScreen ? actionFor({ kind: "pane", paneId: activePane.id, edge: "right" }) : undefined,
      workspaces,
    };
  }, [agentDragPayload, run]);

  return { tabDragPayload, agentDragPayload, agentMoves };
}
