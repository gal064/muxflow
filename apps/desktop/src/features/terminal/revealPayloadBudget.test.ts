/** Payload budgets from the real Rust store, consumed by the desktop hub/reducer. */
import { describe, expect, it } from "vitest";
import type { TerminalEvent } from "./api";
import { reducePaneReveal, type PaneRevealState } from "./PaneRevealState";
import { RustPaneHost } from "./testing/RustPaneHost";
import { TerminalEventHub } from "./TerminalEventHub";

/** Every recovery byte the host put on the wire for this pane. */
function payloadBytes(events: readonly TerminalEvent[]): number {
  return events.reduce(
    (total, event) =>
      event.kind === "paneResource" ? total + event.rawTail.byteLength : total,
    0,
  );
}

describe("what one hide and reveal puts on the wire", () => {
  it("costs nothing at all for a pane that printed nothing while hidden", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new RustPaneHost(hub);
    host.output("%1", "the screen the renderer is holding");
    host.seedOnRequest("%1");
    const held = host.generation;

    host.hide("%1", { terminalEpoch: 7, outputGeneration: held });
    host.reveal("%1", true, { terminalEpoch: 7, outputGeneration: held });

    expect(payloadBytes(delivered)).toBe(0);
    // And the pane is nonetheless ready: the renderer restores from its own
    // cache, and the empty tail is the acknowledgement that it may.
    const answer = delivered.filter((event) => event.kind === "paneResource").at(-1);
    expect(answer?.kind).toBe("paneResource");
    const state: PaneRevealState = { ready: false, hasLocalState: true };
    const result = reducePaneReveal(state, answer as Extract<TerminalEvent, { paneId: string }>);
    expect(result.state.ready).toBe(true);
  });

  it.each(["partial", "at bound"])("costs exactly the hidden output (%s)", (size) => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new RustPaneHost(hub);
    host.output("%1", "visible");
    host.seedOnRequest("%1");
    const held = host.generation;

    host.hide("%1", { terminalEpoch: 7, outputGeneration: held });
    const bytes = size === "partial" ? 4 * 1024 : host.revealTailBound;
    host.output("%1", "x".repeat(bytes));
    host.reveal("%1", true, { terminalEpoch: 7, outputGeneration: held });

    expect(payloadBytes(delivered)).toBe(bytes);
    expect(delivered.at(-1)).toMatchObject({ kind: "paneResource", resumeFromRenderer: true, requiresSeed: false });
  });

  it.each(["x", "é"])("bounds hidden output by bytes and seeds on overflow (%s)", (character) => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new RustPaneHost(hub);
    host.output("%1", "visible");
    host.seedOnRequest("%1");
    const held = host.generation;

    host.hide("%1", { terminalEpoch: 7, outputGeneration: held });
    const width = new TextEncoder().encode(character).byteLength;
    host.output("%1", character.repeat(host.revealTailBound / width + 1));
    host.reveal("%1", true, { terminalEpoch: 7, outputGeneration: held });

    expect(payloadBytes(delivered)).toBe(0);
    const answer = delivered.filter((event) => event.kind === "paneResource").at(-1);
    expect(answer).toMatchObject({ kind: "paneResource", requiresSeed: true });
  });

  it("surfaces Rust's checkpoint refusal and can still complete a valid handoff", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new RustPaneHost(hub);
    host.seedOnRequest("%1");
    const held = host.generation;
    expect(() => host.hide("%1", { terminalEpoch: 0, outputGeneration: held }))
      .toThrow("terminal visibility checkpoint epoch must be non-zero");
    host.hide("%1", { terminalEpoch: 7, outputGeneration: held });
    host.output("%1", "tail");
    host.reveal("%1", true, { terminalEpoch: 7, outputGeneration: held });
    expect(payloadBytes(delivered)).toBe(4);
    expect(delivered.at(-1)).toMatchObject({ kind: "paneResource", resumeFromRenderer: true });
  });

  it("sends a lagging renderer's tail only on reveal, even after repeated hides", () => {
    const hub = new TerminalEventHub();
    const delivered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => delivered.push(event));
    const host = new RustPaneHost(hub);
    host.seedOnRequest("%1");
    const checkpoint = { terminalEpoch: 7, outputGeneration: host.generation };
    host.output("%1", "pending");
    host.hide("%1", checkpoint);
    host.output("%1", "hidden");
    host.hide("%1", checkpoint);
    expect(payloadBytes(delivered)).toBe(0);
    host.reveal("%1", true, checkpoint);
    expect(payloadBytes(delivered)).toBe(13);
    const answer = delivered.at(-1);
    expect(answer).toMatchObject({ kind: "paneResource", resumeFromRenderer: true });
    if (answer?.kind !== "paneResource") throw new Error("missing reveal answer");
    expect(new TextDecoder().decode(answer.rawTail)).toBe("pendinghidden");
  });
});
