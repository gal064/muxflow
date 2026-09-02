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
  | {
      /**
       * This pane's own hide, echoed back into a mount that is still waiting for
       * the answer to the reveal it sent. Nothing to draw and nothing to ask
       * for — that answer is the authoritative one — but a pane with nothing on
       * it still needs a bound on how long it waits.
       */
      kind: "awaitAnswer";
      /** Whether this terminal has a screen up while the answer is outstanding. */
      showingScreen: boolean;
    }
  | {
      /**
       * The same echo, arriving at a pane that is already drawing. Ignored, and
       * recorded: acting on it wiped a correct screen.
       */
      kind: "hideEchoAfterReady";
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
  // The echo of this pane's own hide, replayed into its next mount.
  //
  // Never authoritative, whatever this pane's state: `hiddenBuffered` is only
  // ever produced by a hide, because every path `reveal` can return by stamps
  // `Visible`. It reaches a mounted pane at all because the hide is sent after
  // this pane's own events are unsubscribed, so its acknowledgement lands in the
  // hub's dormant backlog and is replayed into the next mount.
  if (event.state === "hiddenBuffered") {
    // Already drawing. The dead end below would blank a screen that is correct
    // — a pane the user is looking at, going dark on the acknowledgement of a
    // hide it has already come back from. Ignored, and recorded by the owner,
    // because a hide landing behind a reveal is worth being able to see.
    if (state.ready) return { state, effect: { kind: "hideEchoAfterReady" } };
    // Still waiting for the reveal's own answer — a resume or a seed. Asking for
    // a capture here is one wasted seed per switch, and blanking on it is a
    // visible blink for a pane that had a perfectly good screen. What is owed is
    // a bound on the wait, and only for a pane that has nothing up meanwhile.
    return { state, effect: { kind: "awaitAnswer", showingScreen: state.hasLocalState } };
  }
  // Everything left is an answer this pane cannot act on: a state this build has
  // no rule for, which is what an unset or newer `PaneResourceState` decodes as
  // (`unspecified`). Returning `none` here left the pane `ready: false` for the
  // rest of its life — every later output deferred and never written, no
  // watchdog reason, no diagnostic, nothing in the journal — which is the
  // quietest way a pane can stay blank. It is seed debt, and the host plainly
  // does not believe it owes one, so this side asks.
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
