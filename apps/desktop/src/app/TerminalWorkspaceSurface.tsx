import type { MutableRefObject, PointerEvent } from "react";
import type { Pane, TmuxSnapshot, Window } from "./types";
import { resolveTerminalDestination } from "./paneRouting";
import { renderedPaneStyle, type WindowGrid } from "../features/terminal/layout";
import { TerminalPane, type TerminalPaneController } from "../features/terminal/TerminalPane";
import type { TerminalEventHub } from "../features/terminal/TerminalEventHub";
import type { TerminalInput, TerminalSize } from "../features/terminal/TerminalRenderer";
import type { TauriTerminalTransferClient } from "../features/terminal/terminalTransferApi";
import type { TerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import type { TerminalTransferConnectionScope } from "../features/terminal/terminalTransfers";
import type { TmuxAction } from "../features/tmux/actions";

type TerminalWorkspaceSurfaceProps = {
  activePane?: Pane;
  activeWindow?: Window;
  clientId?: string;
  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  grid: WindowGrid;
  hub: TerminalEventHub;
  mountedPanes: Pane[];
  panes: Pane[];
  snapshot: TmuxSnapshot;
  terminalTransferClient: TauriTerminalTransferClient;
  terminalTransferRegistry: TerminalTransferRegistry;
  terminalTransferScope?: TerminalTransferConnectionScope;
  beginDividerDrag(event: PointerEvent<HTMLElement>, pane: Pane, axis: "horizontal" | "vertical"): void;
  handleInput(paneId: string, input: TerminalInput): void;
  handleResize(pane: Pane, size: TerminalSize): void;
  performAction(action: TmuxAction): Promise<boolean>;
  setStatus(message: string): void;
};

export function TerminalWorkspaceSurface(props: TerminalWorkspaceSurfaceProps) {
  const { activePane, activeWindow, grid } = props;
  return <div className="terminal-window" aria-label={activeWindow ? `Terminal tab ${activeWindow.name}` : "Terminal"}>
    {props.mountedPanes.map((pane) => <div className={pane.active ? "pane-frame active" : "pane-frame"} style={renderedPaneStyle(pane, grid, Boolean(activeWindow?.zoomed))} key={pane.id}>
      <div className="pane-label">{pane.id} · {pane.currentCommand}</div>
      <TerminalPane
        clientId={props.clientId}
        pane={pane}
        hub={props.hub}
        onController={(paneId, controller) => { if (controller) props.controllers.current.set(paneId, controller); else props.controllers.current.delete(paneId); }}
        onDiagnostic={props.setStatus}
        onFocus={(paneId) => { if (paneId !== activePane?.id) void props.performAction({ kind: "focusPane", paneId }); }}
        onInput={props.handleInput}
        onResize={props.handleResize}
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
    </div>)}
    {props.panes.length === 0 && <div className="empty">No tmux panes in this terminal window.</div>}
  </div>;
}
