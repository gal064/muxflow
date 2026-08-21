import { paneRecoveryPlan } from "./PaneRecovery";
import type { TerminalEvent } from "./api";
import type { OwnedTerminalBytes } from "./TerminalBytes";

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
  | { kind: "seed"; data: OwnedTerminalBytes }
  | { kind: "output"; data: OwnedTerminalBytes }
  | { kind: "deferOutput"; data: OwnedTerminalBytes }
  | {
      kind: "restore";
      serialized: string;
      rawTail: OwnedTerminalBytes;
      snapshotGeneration: number;
      tailThroughGeneration: number;
    }
  | {
      kind: "awaitSeed";
      reason: string;
      requestSeed: boolean;
      /**
       * Set only when this seed debt came from a handshake answer in a state
       * this reducer has no rule for. The owner journals it; nothing else
       * behaves differently, because the recovery is the same either way.
       */
      unusableState?: string;
    }
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
  // Everything left is an answer this pane cannot act on: a state this build
  // has no rule for (an unset or newer `PaneResourceState` decodes as
  // `unspecified`), or one that says the host is holding this pane while
  // handing back nothing to hold it with. Returning `none` here left the pane
  // `ready: false` for the rest of its life — every later output deferred and
  // never written, no watchdog reason, no diagnostic, nothing in the journal
  // — which is the quietest way a pane can stay blank. It is seed debt, and
  // the host plainly does not believe it owes one, so this side asks.
  return {
    state: { ready: false, hasLocalState: false },
    effect: {
      kind: "awaitSeed",
      reason: `The host answered this pane's reveal with no usable renderer state (${event.state})`,
      requestSeed: true,
      unusableState: event.state,
    },
  };
}
