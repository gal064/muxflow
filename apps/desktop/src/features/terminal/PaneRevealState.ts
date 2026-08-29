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
      /**
       * Draw what you are already holding, then these bytes. Carries no screen
       * because the renderer kept its own, and an empty `rawTail` is still an
       * effect: it is the ordered barrier that acknowledges the reveal.
       */
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
  if (recovery.kind === "resume") {
    // The host verified its own record of the handoff, not this renderer's
    // cache. A pane holding no screen has nothing for the tail to continue, so
    // it asks for the photograph instead.
    if (!state.hasLocalState) {
      return {
        state: { ready: false, hasLocalState: false },
        effect: {
          kind: "awaitSeed",
          reason: "The host resumed this pane from a screen it is no longer holding",
          requestSeed: true,
        },
      };
    }
    return {
      state: { ready: true, hasLocalState: true },
      effect: {
        kind: "resume",
        rawTail: recovery.rawTail,
        snapshotGeneration: recovery.snapshotGeneration,
        tailThroughGeneration: recovery.tailThroughGeneration,
      },
    };
  }
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
  // The echo of this pane's own hide, replayed into its next mount: the host
  // acknowledged the handoff with an empty `hiddenBuffered` answer, and the
  // renderer reading it is showing the screen it kept and is still waiting for
  // the answer to the reveal it has sent. That answer is the authoritative one
  // — a resume or a seed — so asking for a capture on the echo is one wasted
  // seed per switch for a pane that is not even blank. Narrow deliberately: a
  // pane that is already `ready` has no reveal outstanding, so the same shape
  // arriving there is the host parking a pane this side believes it is
  // drawing, which is the dead end below.
  if (!state.ready && state.hasLocalState && event.state === "hiddenBuffered") {
    return { state, effect: { kind: "none" } };
  }
  // Everything left is an answer this pane cannot act on: a state this build
  // has no rule for (an unset or newer `PaneResourceState` decodes as
  // `unspecified`), or one that says the host is holding this pane while
  // handing back nothing to hold it with — and nothing on this side to show
  // meanwhile. Returning `none` here left the pane `ready: false` for the rest
  // of its life — every later output deferred and never written, no watchdog
  // reason, no diagnostic, nothing in the journal — which is the quietest way
  // a pane can stay blank. It is seed debt, and the host plainly does not
  // believe it owes one, so this side asks.
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
