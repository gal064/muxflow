import type { Pane, SessionState } from "../../store/sessionStore";

/** §9.4: the window's active pane; if none is marked active, the lowest index. */
export function activePaneForWindow(state: Pick<SessionState, "panes">, windowId: string): Pane | undefined {
  const panes = Object.values(state.panes).filter((p) => p.windowId === windowId);
  return panes.find((p) => p.active) ?? panes.sort((a, b) => a.index - b.index)[0];
}
