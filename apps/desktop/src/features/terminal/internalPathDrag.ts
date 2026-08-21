import { shellEscapePath } from "./terminalTransfers";

export const INTERNAL_PATH_DRAG_TYPE = "application/x-muxflow-internal-path+json";

export interface InternalPathDragSource {
  serverIdentity: string;
  path: string;
}

type InternalPathDrop =
  | { kind: "absent" }
  | { kind: "accepted"; path: string; shellText: string }
  | { kind: "rejected"; reason: string };

export function writeInternalPathDrag(
  transfer: Pick<DataTransfer, "effectAllowed" | "setData">,
  source: InternalPathDragSource,
): void {
  assertSource(source);
  transfer.effectAllowed = "copy";
  transfer.setData(INTERNAL_PATH_DRAG_TYPE, JSON.stringify({ version: 1, ...source }));
}

export function readInternalPathDrop(
  transfer: Pick<DataTransfer, "getData" | "types">,
  targetServerIdentity: string | undefined,
): InternalPathDrop {
  if (!transfer.types || !Array.from(transfer.types).includes(INTERNAL_PATH_DRAG_TYPE)) return { kind: "absent" };
  if (!targetServerIdentity) return { kind: "rejected", reason: "The target terminal is disconnected." };
  try {
    const value: unknown = JSON.parse(transfer.getData(INTERNAL_PATH_DRAG_TYPE));
    if (!value || typeof value !== "object") throw new Error("payload is not an object");
    const payload = value as Partial<InternalPathDragSource> & { version?: unknown };
    if (payload.version !== 1 || typeof payload.serverIdentity !== "string" || typeof payload.path !== "string") {
      throw new Error("payload fields are invalid");
    }
    assertSource({ serverIdentity: payload.serverIdentity, path: payload.path });
    if (payload.serverIdentity !== targetServerIdentity) {
      return { kind: "rejected", reason: "Paths can only be dropped into a terminal on the same host." };
    }
    return { kind: "accepted", path: payload.path, shellText: shellEscapePath(payload.path) };
  } catch (error) {
    return { kind: "rejected", reason: `The internal path drag payload is invalid: ${String(error)}` };
  }
}

function assertSource(source: InternalPathDragSource): void {
  if (!source.serverIdentity || source.serverIdentity.includes("\0")) throw new Error("Drag host identity is invalid.");
  if (!isCanonicalAbsolutePath(source.path)) throw new Error("Dragged paths must be canonical absolute paths.");
}

function isCanonicalAbsolutePath(path: string): boolean {
  if (!path.startsWith("/") || path.includes("\0") || path.includes("//")) return false;
  return path.split("/").every((part, index) => index === 0 || (part !== "" && part !== "." && part !== ".."));
}
