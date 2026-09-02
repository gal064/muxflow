import type { TerminalEvent } from "./api";
import type { OwnedTerminalBytes } from "./TerminalBytes";

export type PaneRecoveryPlan =
  | { kind: "none" }
  | { kind: "awaitSeed"; reason: string }
  | {
      kind: "resume";
      rawTail: OwnedTerminalBytes;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    };

export function paneRecoveryPlan(
  event: Extract<TerminalEvent, { kind: "paneResource" }>,
): PaneRecoveryPlan {
  if (event.requiresSeed || event.state === "released") {
    // An empty reason is the ordinary case — the renderer kept nothing and the
    // pane simply seeds — and is not spoken; a host-given reason is.
    return { kind: "awaitSeed", reason: event.recoveryReason };
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
  return { kind: "none" };
}
