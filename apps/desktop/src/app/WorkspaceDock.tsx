import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type DragEvent, type RefObject } from "react";
import { useTransientDrag } from "../features/workspaces/transientDrag";
import { DOCK_MAX_FRACTION, DOCK_MIN_FRACTION, TERMINAL_MIN_WIDTH, type DockSide } from "../features/shell/types";
import { useDragSelector } from "../features/dragDrop/pointerDrag";
import {
  consumeNativeInternalPathDrop,
  internalPathDragLive,
  nativeInternalPathDragClaimed,
  readInternalPathDrop,
  subscribeInternalPathDrag,
  type InternalPathDragTarget,
} from "../features/terminal/internalPathDrag";
import { registerNativeDragDropTarget } from "../features/terminal/nativeDragDropCoordinator";

/** The narrowest a docked document is dragged to, in CSS pixels. */
export const DOCK_MIN_WIDTH = 280;
/** One arrow key's worth of the divider, as a share of the workspace. */
const DOCK_KEY_STEP = 0.02;

/** The CSS custom property both columns are laid out from. */
export function dockWidthStyle(fraction: number): string {
  return `${(fraction * 100).toFixed(3)}%`;
}

/**
 * The docked share a pointer at `clientX` asks for, kept inside the fraction
 * bounds and, where the workspace is wide enough, inside the pixel floors too:
 * the document no narrower than `DOCK_MIN_WIDTH`, the terminal no narrower than
 * the width a rail is never allowed to squeeze it below.
 */
export function dockFractionAt(rect: Pick<DOMRect, "left" | "width">, clientX: number, side: DockSide): number {
  if (rect.width <= 0) return DOCK_MIN_FRACTION;
  const offset = (clientX - rect.left) / rect.width;
  const asked = side === "right" ? 1 - offset : offset;
  const low = Math.max(DOCK_MIN_FRACTION, DOCK_MIN_WIDTH / rect.width);
  const high = Math.min(DOCK_MAX_FRACTION, 1 - TERMINAL_MIN_WIDTH / rect.width);
  // A workspace too narrow for both floors keeps the fraction bounds alone.
  const [min, max] = low <= high ? [low, high] : [DOCK_MIN_FRACTION, DOCK_MAX_FRACTION];
  return Math.min(max, Math.max(min, asked));
}

/**
 * The divider between the terminal and the docked document.
 *
 * A drag previews by writing the width straight onto the workspace element, so
 * the shell does not re-render once per frame; the terminal column's box
 * changes all the same, and `useClientResize` resizes the tmux client from it
 * with the debounce every other resizer already goes through. The width is
 * persisted once, on release.
 */
export function DockResizeHandle(props: {
  content: RefObject<HTMLElement | null>;
  fraction: number;
  side: DockSide;
  onFraction(fraction: number): void;
}) {
  const [displayed, startDrag] = useTransientDrag(props.fraction, props.onFraction);
  const { content } = props;
  useLayoutEffect(() => {
    content.current?.style.setProperty("--dock-width", dockWidthStyle(displayed));
  }, [content, displayed]);
  const clamp = (value: number) => Math.min(DOCK_MAX_FRACTION, Math.max(DOCK_MIN_FRACTION, value));
  return <div
    aria-label="Resize the docked document"
    aria-orientation="vertical"
    aria-valuemax={Math.round(DOCK_MAX_FRACTION * 100)}
    aria-valuemin={Math.round(DOCK_MIN_FRACTION * 100)}
    aria-valuenow={Math.round(displayed * 100)}
    className={`dock-resize ${props.side}`}
    onKeyDown={(event) => {
      const toward = event.key === "ArrowLeft" ? "left" : event.key === "ArrowRight" ? "right" : undefined;
      if (!toward) return;
      event.preventDefault();
      // Moving the divider toward the document's own edge narrows it.
      props.onFraction(clamp(displayed + (toward === props.side ? -DOCK_KEY_STEP : DOCK_KEY_STEP)));
    }}
    onPointerDown={(event) => {
      const rect = content.current?.getBoundingClientRect();
      if (!rect) return;
      // Unlike the rails' handles, this one borders selectable text (a
      // Markdown preview), which the press would otherwise start selecting.
      event.preventDefault();
      startDrag(event, (pointer) => dockFractionAt(rect, pointer.clientX, props.side));
    }}
    role="separator"
    tabIndex={0}
  />;
}

/** A tab dragged over the workspace has nothing else to do there, so its zones are wide. */
const TAB_DOCK_ZONE = 0.25;
/**
 * A file dragged from the explorer still pastes its path into whichever pane
 * it is dropped on, so its zones are a strip at the very edge.
 */
const PATH_DOCK_ZONE = 0.12;
const PATH_DOCK_ZONE_MAX_PX = 120;
/** Above the panes' own native targets, which also claim the edge panes' boxes. */
const DOCK_NATIVE_PRIORITY = 1;

export type DockDragKind = "tab" | "path";

/** How wide each edge's zone is, in CSS pixels, for a workspace `width` wide. */
export function dockZoneWidth(width: number, kind: DockDragKind): number {
  return kind === "tab" ? width * TAB_DOCK_ZONE : Math.min(width * PATH_DOCK_ZONE, PATH_DOCK_ZONE_MAX_PX);
}

/** The edge zone a point falls in, or none. */
export function dockSideAt(
  rect: Pick<DOMRect, "left" | "top" | "width" | "height">,
  point: { x: number; y: number },
  kind: DockDragKind,
): DockSide | undefined {
  if (point.y < rect.top || point.y >= rect.top + rect.height) return undefined;
  const zone = dockZoneWidth(rect.width, kind);
  if (point.x >= rect.left && point.x < rect.left + zone) return "left";
  if (point.x < rect.left + rect.width && point.x >= rect.left + rect.width - zone) return "right";
  return undefined;
}

/**
 * The two edges of the workspace a document can be dropped on to dock it.
 *
 * Two kinds of drag reach them. A tab dragged from the strip is a pointer drag
 * (`pointerDrag`), hit-tested against `data-drop-dock`; the zones exist only
 * while one is in flight, so they never sit over the terminal otherwise. A row
 * dragged from the explorer or Git is an HTML5 drag carrying a path
 * (`internalPathDrag`), which arrives either as a DOM drop on the zone or, on
 * macOS, as a native drop routed here ahead of the pane underneath.
 */
export function DockDropZones(props: {
  content: RefObject<HTMLElement | null>;
  /** The share a docked document would take, for the hint. */
  fraction: number;
  /** The host and server a dragged path must belong to; absent while disconnected. */
  scope?: InternalPathDragTarget;
  onDockPath(path: string, side: DockSide): void;
  onError(message: string): void;
}) {
  const tabDrag = useDragSelector((drag) => drag?.payload.kind === "appTab");
  const tabSide = useDragSelector((drag) => drag?.allowed && drag.target?.kind === "dock" ? drag.target.side : undefined);
  const pathDrag = useSyncExternalStore(subscribeInternalPathDrag, internalPathDragLive, () => false);
  const [pathSide, setPathSide] = useState<DockSide>();
  const latest = useRef(props);
  latest.current = props;

  useEffect(() => {
    if (!pathDrag) setPathSide(undefined);
  }, [pathDrag]);

  useEffect(() => {
    const sideAt = (point: { x: number; y: number }) => {
      const rect = latest.current.content.current?.getBoundingClientRect();
      return rect ? dockSideAt(rect, point, "path") : undefined;
    };
    let hovered: DockSide | undefined;
    return registerNativeDragDropTarget({
      // Only our own drags, never files from the desktop: those still upload
      // into the pane they land on.
      owns: (point) => nativeInternalPathDragClaimed() && (hovered = sideAt(point)) !== undefined,
      setDragging: (dragging) => setPathSide(dragging ? hovered : undefined),
      drop: (paths) => {
        const side = hovered;
        if (paths.length > 0 || !side) return;
        const dropped = consumeNativeInternalPathDrop(latest.current.scope);
        if (dropped.kind === "accepted") latest.current.onDockPath(dropped.path, side);
        else if (dropped.kind === "rejected") latest.current.onError(dropped.reason);
      },
      fail: () => undefined,
    }, DOCK_NATIVE_PRIORITY);
  }, []);

  if (!tabDrag && !pathDrag) return null;
  const kind: DockDragKind = tabDrag ? "tab" : "path";
  const side = tabDrag ? tabSide : pathSide;
  const zone = (edge: DockSide) => {
    const over = (event: DragEvent<HTMLElement>) => {
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
      setPathSide(edge);
    };
    return <div
      aria-hidden="true"
      className={`dock-drop-zone ${edge} for-${kind}`}
      data-drop-dock={edge}
      onDragEnter={over}
      onDragOver={over}
      onDragLeave={() => setPathSide((current) => current === edge ? undefined : current)}
      onDrop={(event) => {
        event.preventDefault();
        setPathSide(undefined);
        const dropped = readInternalPathDrop(event.dataTransfer, latest.current.scope);
        if (dropped.kind === "accepted") latest.current.onDockPath(dropped.path, edge);
        else if (dropped.kind === "rejected") latest.current.onError(dropped.reason);
      }}
    />;
  };
  return <>
    {zone("left")}
    {zone("right")}
    {side && <div
      aria-hidden="true"
      className={`dock-drop-hint ${side}`}
      style={{ width: dockWidthStyle(props.fraction) }}
    />}
  </>;
}

/** The docked document's layer inside the workspace, when one is docked. */
function dockedLayer(content: HTMLElement | null): HTMLElement | null {
  return content?.querySelector<HTMLElement>("[data-docked-tab]") ?? null;
}

/** Whether the keyboard is in the docked document, which the terminal must then not take back. */
export function dockHoldsFocus(content: HTMLElement | null): boolean {
  const layer = dockedLayer(content);
  const active = layer?.ownerDocument.activeElement;
  return Boolean(layer && active && layer.contains(active));
}

/**
 * Puts the keyboard in the docked document: its editor where there is one
 * (Monaco's input, CodeMirror's content), otherwise its first focusable
 * control. Returns whether focus landed there.
 */
export function focusDockedDocument(content: HTMLElement | null): boolean {
  const layer = dockedLayer(content);
  if (!layer) return false;
  const target = layer.querySelector<HTMLElement>(
    ".monaco-editor textarea, .monaco-editor .native-edit-context, .cm-content, [tabindex]:not([tabindex='-1'])",
  );
  target?.focus();
  return dockHoldsFocus(content);
}
