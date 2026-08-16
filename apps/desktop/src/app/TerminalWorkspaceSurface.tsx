import { memo, type MutableRefObject, type PointerEvent } from "react";
import type { Pane, Window } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { renderedPaneStyle, type WindowGrid } from "../features/terminal/layout";
import { TerminalPane, type TerminalPaneController } from "../features/terminal/TerminalPane";
import type { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import type { TerminalInput, TerminalMeasurements } from "../features/terminal/TerminalRenderer";
import type { TauriTerminalTransferClient } from "../features/terminal/terminalTransferApi";
import type { TerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import type { TerminalTransferConnectionScope } from "../features/terminal/terminalTransfers";
import type { AgentAttentionRollup } from "../features/agents/types";
import { needsAttention } from "../features/agents/agentsList";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";

type TerminalWorkspaceSurfaceProps = {
  activePane?: Pane;
  activeWindow?: Window;
  clientId?: string;
  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  grid: WindowGrid;
  hub: TerminalEventHub;
  mountedPanes: Pane[];
  /** Per-pane agent state; a pane whose agent wants a human gets the ring. */
  paneAttention?: ReadonlyMap<string, AgentAttentionRollup>;
  panes: Pane[];
  /** Receives the tiled surface element the tmux client size is measured from. */
  surfaceRef: (element: HTMLElement | null) => void;
  terminalTransferClient: TauriTerminalTransferClient;
  terminalTransferRegistry: TerminalTransferRegistry;
  terminalTransferScope?: TerminalTransferConnectionScope;
  beginDividerDrag(event: PointerEvent<HTMLElement>, pane: Pane, axis: "horizontal" | "vertical"): void;
  focusPane(pane: Pane): void;
  handleInput(paneId: string, input: TerminalInput): void;
  /** A terminal reported what it turns pixels into. */
  onMeasurements(measurements: TerminalMeasurements): void;
  performAction(action: TmuxAction): Promise<TmuxActionResult | undefined>;
  setStatus(message: string): void;
};

/** Memoized terminal-only boundary: unrelated root notices/dialogs never revisit live panes. */
export const TerminalWorkspaceSurface = memo(function TerminalWorkspaceSurface(props: TerminalWorkspaceSurfaceProps) {
  const { activePane, activeWindow, grid } = props;
  return <div className="terminal-window" ref={props.surfaceRef} aria-label={activeWindow ? `Terminal tab ${activeWindow.name}` : "Terminal"}>
    {props.mountedPanes.map((pane) => {
      // The floating `%N · cmd` badge is gone: it overlapped the pane's own
      // output, and the pane's identity is already in the tab strip and the
      // accessible name. What remains drawn on a pane is state: 2px accent for
      // focus, 2.5px for an agent that wants a human.
      const attention = props.paneAttention?.get(pane.id)?.state;
      const wantsAttention = attention !== undefined && attention !== "none" && needsAttention(attention);
      return <div
        className={`pane-frame${pane.active ? " active" : ""}${wantsAttention ? " attention" : ""}`}
        key={pane.id}
        style={renderedPaneStyle(pane, grid, Boolean(activeWindow?.zoomed))}
      >
      <TerminalPane
        clientId={props.clientId}
        pane={pane}
        hub={props.hub}
        onController={(paneId, controller) => { if (controller) props.controllers.current.set(paneId, controller); else props.controllers.current.delete(paneId); }}
        onDiagnostic={props.setStatus}
        onFocus={(paneId) => { if (paneId !== activePane?.id) props.focusPane(pane); }}
        onInput={props.handleInput}
        onMeasurements={props.onMeasurements}
        transferClient={props.terminalTransferClient}
        transferRegistry={props.terminalTransferRegistry}
        transferScope={props.terminalTransferScope}
      />
      <div
        aria-label={`Resize ${pane.id} horizontally`}
        aria-orientation="vertical"
        aria-valuemax={100}
        aria-valuemin={0}
        aria-valuenow={grid.width > 0 ? Math.round((pane.width / grid.width) * 100) : 0}
        className="divider-handle horizontal"
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          void props.performAction({ kind: event.key === "ArrowLeft" ? "resizePaneLeft" : "resizePaneRight", paneId: pane.id, resizeCells: 2 });
        }}
        onPointerDown={(event) => props.beginDividerDrag(event, pane, "horizontal")}
        role="separator"
        tabIndex={0}
      />
      <div
        aria-label={`Resize ${pane.id} vertically`}
        aria-orientation="horizontal"
        aria-valuemax={100}
        aria-valuemin={0}
        aria-valuenow={grid.height > 0 ? Math.round((pane.height / grid.height) * 100) : 0}
        className="divider-handle vertical"
        onKeyDown={(event) => {
          if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
          event.preventDefault();
          void props.performAction({ kind: event.key === "ArrowUp" ? "resizePaneUp" : "resizePaneDown", paneId: pane.id, resizeCells: 2 });
        }}
        onPointerDown={(event) => props.beginDividerDrag(event, pane, "vertical")}
        role="separator"
        tabIndex={0}
      />
    </div>;
    })}
    {props.panes.length === 0 && <p className="quiet-empty">No tmux panes in this terminal window.</p>}
  </div>;
});
