import type { TerminalEvent } from "./api";

const serializedSnapshotDecoder = new TextDecoder("utf-8", { fatal: true });

export type PaneRecoveryPlan =
  | { kind: "none" }
  | { kind: "awaitSeed"; reason: string }
  | {
      kind: "restore";
      serialized: string;
      rawTail: Uint8Array;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    };

export function paneRecoveryPlan(
  event: Extract<TerminalEvent, { kind: "paneResource" }>,
): PaneRecoveryPlan {
  if (event.requiresSeed || event.state === "released") {
    return { kind: "awaitSeed", reason: event.recoveryReason || "Renderer state was released" };
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
