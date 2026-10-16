import { useSyncExternalStore, type PointerEvent as ReactPointerEvent } from "react";
import type { PaneEdge } from "../tmux/actions";
import type { DockSide } from "../shell/types";

/**
 * One pointer drag of a tab or a pane (an agent row, or a pane grabbed with
 * the link modifier) onto a tab, the tab strip, a workspace row, or a pane
 * edge.
 *
 * Pointer capture rather than HTML5 drag and drop: Tauri's native drag/drop
 * routing intercepts HTML5 drops (see `nativeDragDropCoordinator`), and a
 * captured pointer keeps the gesture whole however far it travels. Captured
 * events are retargeted to the source, so the target is hit-tested from the
 * pointer position against `data-drop-*` attributes.
 *
 * The state lives in one module-level store, read through
 * `useSyncExternalStore` with narrow selectors, so a drag re-renders only the
 * markers it changes and never the memoized terminal surface.
 */

export type DragPayload =
  | { kind: "terminalTab"; hostProfileId: string; sessionId: string; windowId: string; label: string }
  | { kind: "appTab"; hostProfileId: string; sessionId: string; tabId: string; label: string }
  | { kind: "pane"; hostProfileId: string; sessionId: string; windowId: string; paneId: string; label: string };

export type DropTarget =
  /** `workspace` is the strip's own, for a pane dropped on a tab: it becomes a tab of that workspace. */
  | { kind: "tab"; tabKey: string; side: "before" | "after"; workspace?: { hostProfileId: string; sessionId: string } }
  | { kind: "workspace"; hostProfileId: string; sessionId: string }
  | { kind: "pane"; paneId: string; edge: PaneEdge }
  /** An edge of the workspace, where a document docks beside the terminal. */
  | { kind: "dock"; side: DockSide };

/** What a drop on a target would do; `run` is only called on release. */
export type DropOutcome = { kind: "run"; run(): void } | { kind: "refuse"; reason: string };

export type DropResolver = (payload: DragPayload, target: DropTarget) => DropOutcome | undefined;

export interface DragState {
  payload: DragPayload;
  x: number;
  y: number;
  target?: DropTarget;
  /** Whether releasing here would do something; markers are drawn only then. */
  allowed: boolean;
}

/** Movement before a press becomes a drag, so clicks keep working. */
export const DRAG_THRESHOLD_PX = 4;
/** Share of a pane, from each edge, that counts as that edge; the middle is no drop. */
export const PANE_EDGE_ZONE = 0.3;

let state: DragState | undefined;
let lastEndedAt = Number.NEGATIVE_INFINITY;
let resolver: DropResolver | undefined;
let notify: ((reason: string) => void) | undefined;
const listeners = new Set<() => void>();

function publish(next: DragState | undefined): void {
  state = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The one place drops are decided; App installs it with the live topology. */
export function setDropResolver(next: DropResolver | undefined, onRefuse?: (reason: string) => void): void {
  resolver = next;
  notify = onRefuse;
}

export function pointerDragActive(): boolean {
  return state !== undefined;
}

/** When the last drag ended, on `performance.now()`'s clock. */
export function pointerDragEndedAt(): number {
  return lastEndedAt;
}

export function currentDragState(): DragState | undefined {
  return state;
}

/** Re-renders only when the selected value changes; select primitives. */
export function useDragSelector<T>(select: (current: DragState | undefined) => T): T {
  return useSyncExternalStore(subscribe, () => select(state), () => select(undefined));
}

/** The edge a point falls on, or none in the pane's middle. */
export function paneEdgeAt(rect: Pick<DOMRect, "left" | "top" | "width" | "height">, x: number, y: number): PaneEdge | undefined {
  if (rect.width <= 0 || rect.height <= 0) return undefined;
  const dx = (x - rect.left) / rect.width;
  const dy = (y - rect.top) / rect.height;
  const distances: [PaneEdge, number][] = [["left", dx], ["right", 1 - dx], ["top", dy], ["bottom", 1 - dy]];
  const [edge, distance] = distances.reduce((best, entry) => entry[1] < best[1] ? entry : best);
  return distance <= PANE_EDGE_ZONE ? edge : undefined;
}

function workspaceOf(element: Element): { hostProfileId: string; sessionId: string } | undefined {
  const workspace = element.closest<HTMLElement>("[data-drop-workspace]");
  return workspace?.dataset.dropWorkspace && workspace.dataset.dropHost
    ? { hostProfileId: workspace.dataset.dropHost, sessionId: workspace.dataset.dropWorkspace }
    : undefined;
}

export function dropTargetAt(x: number, y: number): DropTarget | undefined {
  for (const element of document.elementsFromPoint(x, y)) {
    // Drawn over the panes, and only during a drag that can dock, so a dock
    // zone wins over the pane edge under it.
    const dock = element.closest<HTMLElement>("[data-drop-dock]")?.dataset.dropDock;
    if (dock === "left" || dock === "right") return { kind: "dock", side: dock };
    const tab = element.closest<HTMLElement>("[data-drop-tab]");
    if (tab?.dataset.dropTab) {
      const rect = tab.getBoundingClientRect();
      const workspace = workspaceOf(tab);
      return {
        kind: "tab",
        tabKey: tab.dataset.dropTab,
        side: x < rect.left + rect.width / 2 ? "before" : "after",
        ...(workspace ? { workspace } : {}),
      };
    }
    const workspace = workspaceOf(element);
    if (workspace) return { kind: "workspace", ...workspace };
    const pane = element.closest<HTMLElement>("[data-drop-pane]");
    if (pane?.dataset.dropPane) {
      const edge = paneEdgeAt(pane.getBoundingClientRect(), x, y);
      return edge ? { kind: "pane", paneId: pane.dataset.dropPane, edge } : undefined;
    }
  }
  return undefined;
}

function sameTarget(left: DropTarget | undefined, right: DropTarget | undefined): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

/**
 * Arms a drag from a primary-button press. Nothing happens until the pointer
 * travels `DRAG_THRESHOLD_PX`, so the press stays an ordinary click (and a
 * Shift-click stays a pin) when it does not.
 */
export function beginPointerDrag(event: ReactPointerEvent<HTMLElement>, payload: DragPayload | undefined): void {
  if (!payload || event.button !== 0 || state) return;
  const source = event.currentTarget;
  // Read before the press's own default moves focus onto the source.
  const focusedAtPress = document.activeElement;
  const pointerId = event.pointerId;
  const startX = event.clientX;
  const startY = event.clientY;
  let started = false;
  let frame: number | undefined;
  let latest = { x: startX, y: startY };

  const update = () => {
    frame = undefined;
    if (!state) return;
    const target = dropTargetAt(latest.x, latest.y);
    const changed = !sameTarget(target, state.target);
    const allowed = changed
      ? Boolean(target && resolver?.(payload, target)?.kind === "run")
      : state.allowed;
    publish({ payload, x: latest.x, y: latest.y, target, allowed });
  };
  const move = (pointer: PointerEvent) => {
    if (pointer.pointerId !== pointerId) return;
    latest = { x: pointer.clientX, y: pointer.clientY };
    // A release this gesture never saw: a later plain move is not a drag.
    if ((pointer.buttons & 1) === 0) {
      teardown();
      return;
    }
    if (!started) {
      if (Math.hypot(latest.x - startX, latest.y - startY) < DRAG_THRESHOLD_PX) return;
      started = true;
      source.setPointerCapture?.(pointerId);
      document.body.dataset.pointerDrag = "true";
      publish({ payload, x: latest.x, y: latest.y, allowed: false });
    }
    if (frame === undefined) frame = window.requestAnimationFrame(update);
  };
  const teardown = (buttonStillHeld = false) => {
    if (frame !== undefined) window.cancelAnimationFrame(frame);
    window.removeEventListener("lostpointercapture", cancel);
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", release);
    window.removeEventListener("pointercancel", cancel);
    window.removeEventListener("keydown", escape, true);
    if (source.hasPointerCapture?.(pointerId)) source.releasePointerCapture(pointerId);
    delete document.body.dataset.pointerDrag;
    if (started) {
      restoreFocus(source, focusedAtPress);
      if (buttonStillHeld) suppressClickAfterRelease(pointerId);
      else suppressNextClick();
      lastEndedAt = performance.now();
      publish(undefined);
    }
  };
  const release = (pointer: PointerEvent) => {
    if (pointer.pointerId !== pointerId) return;
    const target = started ? dropTargetAt(pointer.clientX, pointer.clientY) : undefined;
    teardown();
    if (!target) return;
    const outcome = resolver?.(payload, target);
    if (outcome?.kind === "run") outcome.run();
    else if (outcome?.kind === "refuse") notify?.(outcome.reason);
  };
  const cancel = (pointer: PointerEvent) => {
    if (pointer.pointerId === pointerId) teardown();
  };
  const escape = (key: KeyboardEvent) => {
    if (key.key !== "Escape" || !started) return;
    key.preventDefault();
    key.stopPropagation();
    teardown(true);
  };

  // Watched on the window: before capture the pointer may leave the source on
  // its way past the threshold, and captured events bubble up to it anyway.
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", release);
  window.addEventListener("pointercancel", cancel);
  window.addEventListener("keydown", escape, true);
  // On the window rather than the source: a source unmounted mid-drag loses
  // capture to the document, and a drag left armed would turn the next
  // ordinary click into a drop.
  window.addEventListener("lostpointercapture", cancel);
}

/**
 * A drag is not a visit: the press focused the row or tab it started on, and
 * left there the next Space or Enter would activate it — jumping to the agent
 * just dragged away — and typing would not reach the terminal. Focus goes
 * back where it was, or off the source when that was nowhere.
 */
function restoreFocus(source: HTMLElement, focusedAtPress: Element | null): void {
  if (focusedAtPress instanceof HTMLElement && focusedAtPress !== document.body && focusedAtPress.isConnected) {
    focusedAtPress.focus({ preventScroll: true });
  } else if (document.activeElement instanceof HTMLElement && source.contains(document.activeElement)) {
    document.activeElement.blur();
  }
}

/**
 * A captured press and release land on the same element, so the browser
 * follows a drag with a click on its source. That click must not select the
 * tab or navigate to the agent the user just dragged away.
 */
/**
 * Escape ends the drag with the button still down, so the click it would
 * have produced comes later, on release. Wait for that release first.
 */
function suppressClickAfterRelease(pointerId: number): void {
  const released = (event: PointerEvent) => {
    if (event.pointerId !== pointerId) return;
    window.removeEventListener("pointerup", released, true);
    window.removeEventListener("pointercancel", released, true);
    if (event.type === "pointerup") suppressNextClick();
  };
  window.addEventListener("pointerup", released, true);
  window.addEventListener("pointercancel", released, true);
}

function suppressNextClick(): void {
  const swallow = (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    remove();
  };
  const remove = () => window.removeEventListener("click", swallow, true);
  window.addEventListener("click", swallow, true);
  window.setTimeout(remove, 0);
}
