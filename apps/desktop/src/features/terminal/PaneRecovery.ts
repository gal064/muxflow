import type { TerminalEvent } from "./api";
import type { OwnedTerminalBytes } from "./TerminalBytes";

const serializedSnapshotDecoder = new TextDecoder("utf-8", { fatal: true });

export type PaneRecoveryPlan =
  | { kind: "none" }
  | { kind: "awaitSeed"; reason: string }
  | {
      kind: "resume";
      rawTail: OwnedTerminalBytes;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    }
  | {
      kind: "restore";
      serialized: string;
      rawTail: OwnedTerminalBytes;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    };

export function paneRecoveryPlan(
  event: Extract<TerminalEvent, { kind: "paneResource" }>,
): PaneRecoveryPlan {
  if (event.requiresSeed || event.state === "released") {
    return { kind: "awaitSeed", reason: event.recoveryReason || "Renderer state was released" };
  }
  // The flag, never the byte count. The host holds no copy of this pane's
  // screen: a verified reveal answers with the output since the checkpoint,
  // and for a pane that printed nothing while hidden that is zero bytes and
  // still the whole recovery.
  if (event.resumeFromRenderer) {
    return {
      kind: "resume",
      rawTail: event.rawTail,
      snapshotGeneration: event.snapshotGeneration,
      tailThroughGeneration: event.tailThroughGeneration,
    };
  }
  if (event.serializedSnapshot.byteLength === 0) return { kind: "none" };
  try {
    return {
      kind: "restore",
      serialized: serializedSnapshotDecoder.decode(event.serializedSnapshot),
      rawTail: event.rawTail,
      snapshotGeneration: event.snapshotGeneration,
      tailThroughGeneration: event.tailThroughGeneration,
    };
  } catch {
    return { kind: "awaitSeed", reason: "Stored renderer recovery was not valid UTF-8" };
  }
}
