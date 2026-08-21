import { getCurrentWebview, type DragDropEvent } from "@tauri-apps/api/webview";
import { cancelNativeInternalPathDrag, claimNativeInternalPathDrag } from "./internalPathDrag";

type Point = { x: number; y: number };

interface NativeDragDropTarget {
  owns(point: Point): boolean;
  setDragging(dragging: boolean): void;
  drop(paths: string[]): void;
  fail(reason: unknown): void;
}

const targets = new Set<NativeDragDropTarget>();
let listenerGeneration = 0;
let listening = false;
let releaseListener: (() => void) | undefined;

/** Registers one pane with the WebView's single native drag/drop listener. */
export function registerNativeDragDropTarget(target: NativeDragDropTarget): () => void {
  targets.add(target);
  ensureListener();
  return () => {
    targets.delete(target);
    target.setDragging(false);
    if (targets.size === 0) stopListener();
  };
}

function ensureListener(): void {
  if (listening || typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  listening = true;
  const generation = ++listenerGeneration;
  void getCurrentWebview().onDragDropEvent((event) => {
    // Registration resolves asynchronously. StrictMode can retire one
    // listener and install its successor before the first unlisten arrives;
    // stale callbacks must not route the same native drop a second time.
    if (listening && generation === listenerGeneration) routeNativeDragDrop(event.payload);
  }).then((release) => {
    if (!listening || generation !== listenerGeneration) release();
    else releaseListener = release;
  }).catch((reason) => {
    if (generation !== listenerGeneration) return;
    listening = false;
    cancelNativeInternalPathDrag();
    for (const target of targets) target.fail(reason);
  });
}

function stopListener(): void {
  listening = false;
  listenerGeneration += 1;
  cancelNativeInternalPathDrag();
  releaseListener?.();
  releaseListener = undefined;
}

function routeNativeDragDrop(payload: DragDropEvent): void {
  if (payload.type === "leave") {
    cancelNativeInternalPathDrag();
    for (const target of targets) target.setDragging(false);
    return;
  }

  const owner = [...targets].find((target) => target.owns(payload.position));
  if (payload.type === "enter" && payload.paths.length === 0) claimNativeInternalPathDrag();
  if (payload.type !== "drop") {
    for (const target of targets) target.setDragging(target === owner);
    return;
  }

  for (const target of targets) target.setDragging(false);
  if (!owner) {
    cancelNativeInternalPathDrag();
    return;
  }
  if (payload.paths.length > 0) cancelNativeInternalPathDrag();
  owner.drop(payload.paths);
}
