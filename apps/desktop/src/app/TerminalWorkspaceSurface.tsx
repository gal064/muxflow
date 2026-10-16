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
import type { Platform } from "../commands/registry";
import { needsAttention } from "../features/agents/agentsList";
import { stripAgentStatusGlyphs } from "../features/agents/agentLabels";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import { beginPointerDrag, useDragSelector, type DragPayload } from "../features/dragDrop/pointerDrag";
import { isTerminalLinkActivation } from "../features/terminal/terminalLinkActivation";
import { usePaneHoverFocus } from "./paneHoverFocus";

type TerminalWorkspaceSurfaceProps = {
  /** Whether the terminal layer is on screen rather than covered by a file. */
  visible?: boolean;
  activePane?: Pane;
  activeWindow?: Window;
  appFocused: boolean;
  /** See `TerminalPane`'s `yieldFocus`; must be stable, or the memo here is defeated. */
  yieldFocus?(): boolean;
  /** A document is docked beside the panes, so even a lone pane has somewhere for hover to come from. */
  docked?: boolean;
  /** The active host's profile id; scopes every pane's screen cache. */
  cacheScope: string;
  clientId?: string;
  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  grid: WindowGrid;
  hub: TerminalEventHub;
  mountedPanes: Pane[];
  /** Per-pane agent state; a pane whose agent wants a human gets the ring. */
  paneAttention?: ReadonlyMap<string, AgentAttentionRollup>;
  panes: Pane[];
  copyOnSelect?: boolean;
  cleanWrappedCommands?: boolean;
  terminalApplicationClipboard?: boolean;
  terminalFontSize: number;
  /** Focus-follows-mouse, already off while a dialog or menu is open or the app is in the background. */
  hoverFocus?: boolean;
  platform?: Platform;
  /** What a pane grabbed with the link modifier carries; nothing while the host cannot be changed. */
  paneDragPayload?(pane: Pane): DragPayload | undefined;
  /** Receives the tiled surface element the tmux client size is measured from. */
  surfaceRef: (element: HTMLElement | null) => void;
  terminalTransferClient: TauriTerminalTransferClient;
  terminalTransferRegistry: TerminalTransferRegistry;
  terminalTransferScope?: TerminalTransferConnectionScope;
  beginDividerDrag(event: PointerEvent<HTMLElement>, pane: Pane, axis: "horizontal" | "vertical"): void;
  focusPane(pane: Pane): void;
  handleInput(paneId: string, input: TerminalInput): void;
  /** A physical key was pressed in a pane, which no `handleInput` can tell. */
  handleKeyActivity(paneId: string): void;
  /** The user pressed inside a pane, including one that was already focused. */
  handlePointerActivity(paneId: string): void;
  /** A terminal reported what it turns pixels into. */
  onMeasurements(measurements: TerminalMeasurements): void;
  onOpenFilePath(paneId: string, path: string): void;
  /** How long one write took to reach the screen — journal-only, throttled. */
  onPaintSample?(paneId: string, ms: number): void;
  performAction(action: TmuxAction): Promise<TmuxActionResult | undefined>;
  setStatus(message: string): void;
};

/**
 * The half of a pane a drop would fill. Subscribed per pane, selecting only
 * its own edge, so a drag re-renders this marker and never the terminal.
 */
function PaneDropOverlay(props: { paneId: string }) {
  const edge = useDragSelector((drag) => drag?.allowed && drag.target?.kind === "pane" && drag.target.paneId === props.paneId
    ? drag.target.edge
    : undefined);
  return edge ? <div aria-hidden="true" className={`pane-drop-hint ${edge}`} /> : null;
}

/** Memoized terminal-only boundary: unrelated root notices/dialogs never revisit live panes. */
export const TerminalWorkspaceSurface = memo(function TerminalWorkspaceSurface(props: TerminalWorkspaceSurfaceProps) {
  const { activePane, activeWindow, grid } = props;
  const hasMultiplePanes = props.mountedPanes.length > 1;
  const hoverHandlers = usePaneHoverFocus({
    enabled: Boolean(props.hoverFocus) && (hasMultiplePanes || Boolean(props.docked)),
    dockHoldsFocus: props.yieldFocus,
    activePaneId: activePane?.id,
    focus: (paneId) => props.controllers.current.get(paneId)?.focus(),
  });
  /**
   * The link modifier (⌘ on macOS, Ctrl elsewhere) grabs the pane itself.
   * Caught before the terminal sees the press, so no selection starts and a
   * program reading the mouse gets no click; over a link the press is left
   * alone and opens it, the way that modifier always has. The grab does not
   * focus the pane: focus stays on the pane you were in, unless that is the
   * pane you move.
   */
  const beginPaneDrag = (event: PointerEvent<HTMLElement>, pane: Pane) => {
    if (event.button !== 0 || !isTerminalLinkActivation(event, props.platform ?? "linux")) return;
    if (!(event.target instanceof Element)) return;
    // The pane's own controls (dividers, the find bar, the new-output button)
    // keep their modified clicks.
    if (event.target.closest(".divider-handle, .xterm-cursor-pointer, .terminal-search, button")) return;
    const payload = props.paneDragPayload?.(pane);
    if (!payload) return;
    event.preventDefault();
    event.stopPropagation();
    beginPointerDrag(event, payload);
  };
  return <div className="terminal-window" ref={props.surfaceRef} aria-label={activeWindow ? `Terminal tab ${stripAgentStatusGlyphs(activeWindow.name)}` : "Terminal"}>
    {props.mountedPanes.map((pane) => {
      // The floating `%N · cmd` badge is gone: it overlapped the pane's own
      // output, and the pane's identity is already in the tab strip and the
      // accessible name. What remains drawn on a pane is state: 2px accent for
      // focus, 2.5px for an agent that wants a human.
      const attention = props.paneAttention?.get(pane.id)?.state;
      const wantsAttention = attention !== undefined && attention !== "none" && needsAttention(attention);
      const zoomed = Boolean(activeWindow?.zoomed);
      const hasRightSeparator = hasMultiplePanes && !zoomed && pane.left + pane.width < grid.width;
      const hasBottomSeparator = hasMultiplePanes && !zoomed && pane.top + pane.height < grid.height;
      return <div
        className={`pane-frame${pane.active ? " active" : ""}${wantsAttention ? " attention" : ""}`}
        data-drop-pane={pane.id}
        key={pane.id}
        style={renderedPaneStyle(pane, grid, zoomed)}
        {...hoverHandlers(pane.id)}
        onPointerDownCapture={(event) => beginPaneDrag(event, pane)}
      >
      <TerminalPane
        visible={props.visible}
        appFocused={props.appFocused}
        yieldFocus={props.yieldFocus}
        cacheScope={props.cacheScope}
        clientId={props.clientId}
        pane={pane}
        copyOnSelect={props.copyOnSelect ?? true}
        cleanWrappedCommands={props.cleanWrappedCommands ?? true}
        terminalApplicationClipboard={props.terminalApplicationClipboard ?? true}
        terminalFontSize={props.terminalFontSize}
        platform={props.platform ?? "linux"}
        hub={props.hub}
        onController={(paneId, controller) => { if (controller) props.controllers.current.set(paneId, controller); else props.controllers.current.delete(paneId); }}
        onDiagnostic={props.setStatus}
        onFocus={(paneId) => { if (paneId !== activePane?.id) props.focusPane(pane); }}
        onInput={props.handleInput}
        onKeyActivity={props.handleKeyActivity}
        onPointerActivity={props.handlePointerActivity}
        onMeasurements={props.onMeasurements}
        onOpenFilePath={props.onOpenFilePath}
        onPaintSample={props.onPaintSample}
        transferClient={props.terminalTransferClient}
        transferRegistry={props.terminalTransferRegistry}
        transferScope={props.terminalTransferScope}
      />
      {hasRightSeparator && <div
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
      />}
      {hasBottomSeparator && <div
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
      />}
      <PaneDropOverlay paneId={pane.id} />
    </div>;
    })}
    {props.panes.length === 0 && <p className="quiet-empty">No tmux panes in this terminal window.</p>}
  </div>;
});
