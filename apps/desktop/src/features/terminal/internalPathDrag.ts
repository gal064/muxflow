import { shellEscapePath } from "./terminalTransfers";

export const INTERNAL_PATH_DRAG_TYPE = "application/x-muxflow-internal-path+json";

export interface InternalPathDragSource {
  hostProfileId: string;
  serverIdentity: string;
  path: string;
}

export type InternalPathDragTarget = Pick<InternalPathDragSource, "hostProfileId" | "serverIdentity">;

interface ActiveInternalPathDrag {
  gestureId: string;
  nativeClaimed: boolean;
  delivery: "pending" | "delivered";
  source: InternalPathDragSource;
  expiresAt: number;
}

const ACTIVE_DRAG_MILLIS = 30_000;
/** How long an accepted drag waits after dragend for the native drop event. */
const ACCEPTED_DRAG_GRACE_MILLIS = 2_000;
let activeDrag: ActiveInternalPathDrag | undefined;
let nextGestureId = 0;

type InternalPathDrop =
  | { kind: "absent" }
  | { kind: "handled" }
  | { kind: "accepted"; path: string; shellText: string }
  | { kind: "rejected"; reason: string };

export function writeInternalPathDrag(
  transfer: Pick<DataTransfer, "effectAllowed" | "setData">,
  source: InternalPathDragSource,
): void {
  assertSource(source);
  const gestureId = String(++nextGestureId);
  transfer.effectAllowed = "copy";
  transfer.setData(INTERNAL_PATH_DRAG_TYPE, JSON.stringify({ version: 1, gestureId, ...source }));
  // Tauri/Wry consumes HTML drop events on macOS even for an internal drag,
  // but still publishes its native event with an empty path list. Retain the
  // already-validated source just for that drag so the native event can bridge
  // it without trusting an external text payload.
  activeDrag = {
    gestureId,
    nativeClaimed: false,
    delivery: "pending",
    source: { ...source },
    expiresAt: Date.now() + ACTIVE_DRAG_MILLIS,
  };
}

/** Marks an empty-path native enter as the macOS half of our active DOM drag. */
export function claimNativeInternalPathDrag(): boolean {
  const drag = currentActiveDrag();
  if (!drag) return false;
  drag.nativeClaimed = true;
  return true;
}

/** Consumes a claimed native drag once, preserving the normal host validation. */
export function consumeNativeInternalPathDrop(target: InternalPathDragTarget | undefined): InternalPathDrop {
  const drag = currentActiveDrag();
  if (!drag?.nativeClaimed) return { kind: "absent" };
  if (drag.delivery === "delivered") {
    activeDrag = undefined;
    return { kind: "handled" };
  }
  drag.delivery = "delivered";
  return resolveInternalPathSource(drag.source, target);
}

/**
 * dragend. `accepted` is `dropEffect !== "none"`: a target took the drop.
 *
 * A cancelled gesture (Escape, dropped outside every target) is retired at
 * once, so the next unrelated pathless drop — a text selection, an image —
 * cannot paste a retired path. An accepted, claimed gesture stays *pending*:
 * on macOS wry reports the drop through Tauri's async native event, which
 * lands after WebKit's synchronous dragend, so marking it delivered here made
 * the native drop read as already handled and nothing pasted. The wait is
 * bounded tightly instead so a native drop that never arrives cannot leak.
 */
export function finishInternalPathDrag(accepted: boolean): void {
  if (!activeDrag) return;
  if (!accepted || !activeDrag.nativeClaimed) {
    activeDrag = undefined;
    return;
  }
  activeDrag.expiresAt = Date.now() + ACCEPTED_DRAG_GRACE_MILLIS;
}

/**
 * Drops the native lane's claim on the current gesture.
 *
 * A native leave means the pointer left the *window*, not that the drag is
 * over: dragging out and back in is one uninterrupted DOM gesture, and the
 * re-entry re-claims. Retiring the whole record here left the drop that
 * followed with nothing to bridge, so it failed with "The WebView did not
 * provide file paths." Only dragend retires a gesture.
 */
export function releaseNativeInternalPathDrag(): void {
  if (activeDrag) activeDrag.nativeClaimed = false;
}

/** Retires the gesture outright, for a native drop no pane can own. */
export function cancelNativeInternalPathDrag(): void {
  activeDrag = undefined;
}

/** Clears a source that disappeared or changed hosts before dragend arrived. */
export function cancelInternalPathDragSource(target: InternalPathDragTarget): void {
  if (activeDrag
    && activeDrag.source.hostProfileId === target.hostProfileId
    && activeDrag.source.serverIdentity === target.serverIdentity) activeDrag = undefined;
}

export function readInternalPathDrop(
  transfer: Pick<DataTransfer, "getData" | "types">,
  target: InternalPathDragTarget | undefined,
): InternalPathDrop {
  if (!transfer.types || !Array.from(transfer.types).includes(INTERNAL_PATH_DRAG_TYPE)) return { kind: "absent" };
  if (!target) return { kind: "rejected", reason: "The target terminal is disconnected." };
  try {
    const value: unknown = JSON.parse(transfer.getData(INTERNAL_PATH_DRAG_TYPE));
    if (!value || typeof value !== "object") throw new Error("payload is not an object");
    const payload = value as Partial<InternalPathDragSource> & { gestureId?: unknown; version?: unknown };
    if (payload.version !== 1 || typeof payload.gestureId !== "string"
      || typeof payload.hostProfileId !== "string" || typeof payload.serverIdentity !== "string"
      || typeof payload.path !== "string") {
      throw new Error("payload fields are invalid");
    }
    const drag = currentActiveDrag();
    if (!drag || drag.gestureId !== payload.gestureId) {
      return { kind: "rejected", reason: "The internal path drag is no longer active." };
    }
    if (drag.delivery === "delivered") {
      activeDrag = undefined;
      return { kind: "handled" };
    }
    drag.delivery = "delivered";
    return resolveInternalPathSource({ hostProfileId: payload.hostProfileId, serverIdentity: payload.serverIdentity, path: payload.path }, target);
  } catch (error) {
    return { kind: "rejected", reason: `The internal path drag payload is invalid: ${String(error)}` };
  }
}

function currentActiveDrag(): ActiveInternalPathDrag | undefined {
  if (activeDrag && activeDrag.expiresAt >= Date.now()) return activeDrag;
  activeDrag = undefined;
  return undefined;
}

function resolveInternalPathSource(
  source: InternalPathDragSource,
  target: InternalPathDragTarget | undefined,
): InternalPathDrop {
  if (!target) return { kind: "rejected", reason: "The target terminal is disconnected." };
  try {
    assertSource(source);
  } catch (error) {
    return { kind: "rejected", reason: `The internal path drag payload is invalid: ${String(error)}` };
  }
  if (source.hostProfileId !== target.hostProfileId || source.serverIdentity !== target.serverIdentity) {
    return { kind: "rejected", reason: "Paths can only be dropped into a terminal on the same host." };
  }
  return { kind: "accepted", path: source.path, shellText: shellEscapePath(source.path) };
}

function assertSource(source: InternalPathDragSource): void {
  if (!source.hostProfileId || source.hostProfileId.includes("\0")) throw new Error("Drag host profile is invalid.");
  if (!source.serverIdentity || source.serverIdentity.includes("\0")) throw new Error("Drag host identity is invalid.");
  if (!isCanonicalAbsolutePath(source.path)) throw new Error("Dragged paths must be canonical absolute paths.");
}

function isCanonicalAbsolutePath(path: string): boolean {
  if (!path.startsWith("/") || path.includes("\0") || path.includes("//")) return false;
  return path.split("/").every((part, index) => index === 0 || (part !== "" && part !== "." && part !== ".."));
}
