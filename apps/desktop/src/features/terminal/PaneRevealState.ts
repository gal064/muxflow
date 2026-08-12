import { paneRecoveryPlan } from "./PaneRecovery";
import type { TerminalEvent } from "./api";

export interface PaneRevealState {
  ready: boolean;
  hasLocalState: boolean;
}

export function outputAfterRecovery<T extends { generation: number }>(
  output: readonly T[],
  tailThroughGeneration: number,
): T[] {
  return output.filter((item) => item.generation > tailThroughGeneration);
}

export type PaneRevealEffect =
  | { kind: "none" }
  | { kind: "seed"; data: Uint8Array }
  | { kind: "output"; data: Uint8Array }
  | { kind: "deferOutput"; data: Uint8Array }
  | {
      kind: "restore";
      serialized: string;
      rawTail: Uint8Array;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    }
  | { kind: "awaitSeed"; reason: string; requestSeed: boolean }
  | { kind: "diagnostic"; message: string };

export function reducePaneReveal(
  state: PaneRevealState,
  event: Extract<TerminalEvent, { paneId: string }>,
): { state: PaneRevealState; effect: PaneRevealEffect } {
  if (event.kind === "seedDiagnostic") {
    return { state, effect: { kind: "diagnostic", message: event.message } };
  }
  if (event.kind === "seed") {
    return { state: { ready: true, hasLocalState: true }, effect: { kind: "seed", data: event.data } };
  }
  if (event.kind === "output") {
    return { state, effect: state.ready ? { kind: "output", data: event.data } : { kind: "deferOutput", data: event.data } };
  }
  if (event.kind !== "paneResource") return { state, effect: { kind: "none" } };

  const recovery = paneRecoveryPlan(event);
  if (recovery.kind === "restore") {
    return {
      state: { ready: true, hasLocalState: true },
      effect: {
        kind: "restore",
        serialized: recovery.serialized,
        rawTail: recovery.rawTail,
        snapshotGeneration: recovery.snapshotGeneration,
        tailThroughGeneration: recovery.tailThroughGeneration,
      },
    };
  }
  if (recovery.kind === "awaitSeed") {
    return {
      state: { ready: false, hasLocalState: false },
      effect: { kind: "awaitSeed", reason: recovery.reason, requestSeed: !event.requiresSeed },
    };
  }
  if (event.state === "visible" && state.hasLocalState) {
    return { state: { ...state, ready: true }, effect: { kind: "none" } };
  }
  if (event.state === "visible") {
    return {
      state: { ready: false, hasLocalState: false },
      effect: { kind: "awaitSeed", reason: "No recoverable renderer state was available", requestSeed: true },
    };
  }
  return { state, effect: { kind: "none" } };
}
