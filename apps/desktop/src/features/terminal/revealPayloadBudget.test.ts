/**
 * What a workspace switch costs on the wire, as a number.
 *
 * The switch that could not be met on a shaped link was not slow in the host —
 * the host does its part in 6-9 ms — it was late because 170-800 KB of the
 * host's own ordered events were queued ahead of the answer. The largest of
 * them was a pane-resource event carrying the screen the renderer had just
 * uploaded back to the renderer that uploaded it.
 *
 * So the rule this file states: hiding a pane uploads nothing, and revealing
 * one costs the output printed while it was hidden — nothing for an idle pane,
 * and the tail itself up to `REVEAL_TAIL_BOUND`, past which the pane is seeded
 * instead. The `FakeHost` below is the host's half of that rule (§3.2-3.4); the
 * hub and the reveal reducer are the real ones, because the risk this pins is
 * on the desktop side: a zero-byte answer read as "no recovery" is a pane that
 * stays blank forever.
 *
 * Lands with steps 3 and 4.
 */
import { describe, expect, it } from "vitest";
import type { TerminalEvent } from "./api";
import { reducePaneReveal, type PaneRevealState } from "./PaneRevealState";
import { copyTerminalBytes } from "./TerminalBytes";
import { TerminalEventHub } from "./TerminalEventHub";

type Resource = Extract<TerminalEvent, { kind: "paneResource" }>;
/** `resume_from_renderer`, the step 3 proto field (§2). */
type ResumeAnswer = Resource & { resumeFromRenderer: boolean };

/** `REVEAL_TAIL_BOUND` (§3.4): past this a screen is both cheaper and fresher. */
const REVEAL_TAIL_BOUND = 16 * 1024;

const encoder = new TextEncoder();

/**
 * The host, holding no screen: a generation counter, the output a hidden pane
 * has produced, and the checkpoint it recorded when the renderer let go.
 */
class FakeHost {
  #generation = 0;
  #sequence = 0;
  #hidden = false;
  #released = false;
  #tail = "";
  #checkpoint = 0;

  constructor(
    readonly hub: TerminalEventHub,
    readonly paneId = "%1",
  ) {}

  output(text: string): void {
    this.#generation += 1;
    if (!this.#hidden) {
      this.hub.publish({
        kind: "output",
        paneId: this.paneId,
        generation: this.#generation,
        sequence: (this.#sequence += 1),
        data: copyTerminalBytes(encoder.encode(text)),
      });
      return;
    }
    if (this.#tail.length + text.length > REVEAL_TAIL_BOUND) {
      // Not a degradation: a busy hidden pane is expected to outgrow the bound,
      // and the answer to that is a photograph rather than a longer tail.
      this.#released = true;
      this.#tail = "";
      return;
    }
    this.#tail += text;
  }

  /** `hide_with_checkpoint`, with no snapshot to give it. */
  hide(): void {
    this.#hidden = true;
    this.#released = false;
    this.#tail = "";
    this.#checkpoint = this.#generation;
  }

  /**
   * `reveal`: the recorded handoff checkpoint is the authority. It matches only
   * the renderer that actually holds the screen this tail continues; anything
   * else is answered with a seed and no bytes at all.
   */
  reveal(rendererHoldsScreenAt: number): void {
    this.#generation += 1;
    const resumable = !this.#released && rendererHoldsScreenAt === this.#checkpoint;
    const answer: ResumeAnswer = {
      kind: "paneResource",
      paneId: this.paneId,
      state: "visible",
      requiresSeed: !resumable,
      recoveryReason: resumable ? "" : "hidden output outgrew the reveal tail bound",
      generation: this.#generation,
      snapshotGeneration: this.#checkpoint,
      tailThroughGeneration: this.#generation,
      serializedSnapshot: copyTerminalBytes(new Uint8Array()),
      rawTail: copyTerminalBytes(encoder.encode(resumable ? this.#tail : "")),
      sequence: (this.#sequence += 1),
      resumeFromRenderer: resumable,
    };
    this.#hidden = false;
    this.#tail = "";
    this.hub.publish(answer);
  }

  get generation(): number {
    return this.#generation;
  }
}

/** Every recovery byte the host put on the wire for this pane. */
function payloadBytes(events: readonly TerminalEvent[]): number {
  return events.reduce(
    (total, event) =>
      event.kind === "paneResource"
        ? total + event.serializedSnapshot.byteLength + event.rawTail.byteLength
        : total,
    0,
  );
}

describe("what one hide and reveal puts on the wire", () => {
  it.skip("costs nothing at all for a pane that printed nothing while hidden", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new FakeHost(hub);
    host.output("the screen the renderer is holding");
    const held = host.generation;

    host.hide();
    host.reveal(held);

    expect(payloadBytes(delivered)).toBe(0);
    // And the pane is nonetheless ready: the renderer restores from its own
    // cache, and the empty tail is the acknowledgement that it may.
    const answer = delivered.at(-1);
    expect(answer?.kind).toBe("paneResource");
    const state: PaneRevealState = { ready: false, hasLocalState: true };
    const result = reducePaneReveal(state, answer as Extract<TerminalEvent, { paneId: string }>);
    expect(result.state.ready).toBe(true);
  });

  it.skip("costs exactly the output printed while the pane was hidden", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new FakeHost(hub);
    host.output("visible");
    const held = host.generation;

    host.hide();
    host.output("x".repeat(4 * 1024));
    host.reveal(held);

    expect(payloadBytes(delivered)).toBe(4 * 1024);
  });

  it.skip("stops at the bound and answers with a seed instead of a longer tail", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new FakeHost(hub);
    host.output("visible");
    const held = host.generation;

    host.hide();
    host.output("x".repeat(REVEAL_TAIL_BOUND + 1));
    host.reveal(held);

    expect(payloadBytes(delivered)).toBe(0);
    const answer = delivered.at(-1);
    expect(answer).toMatchObject({ kind: "paneResource", requiresSeed: true });
  });
});
