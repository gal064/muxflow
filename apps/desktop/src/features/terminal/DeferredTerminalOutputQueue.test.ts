import { describe, expect, it } from "vitest";
import { ownTerminalBytes } from "./TerminalBytes";
import {
  DeferredTerminalOutputQueue,
  MAX_DEFERRED_OUTPUT_RECORDS,
} from "./DeferredTerminalOutputQueue";

describe("DeferredTerminalOutputQueue", () => {
  it("bounds empty recovery records and latches one overflow episode", () => {
    const queue = new DeferredTerminalOutputQueue();
    for (let generation = 1; generation <= MAX_DEFERRED_OUTPUT_RECORDS; generation += 1) {
      expect(queue.enqueue({ data: ownTerminalBytes(new Uint8Array()), generation })).toBe("accepted");
    }
    expect(queue.enqueue({ data: ownTerminalBytes(new Uint8Array()), generation: 1025 })).toBe("overflow");
    expect(queue.recordCount).toBe(0);
    expect(queue.byteLength).toBe(0);
    expect(queue.enqueue({ data: ownTerminalBytes(new Uint8Array()), generation: 1026 })).toBe("blocked");
  });

  it("resets the overflow latch only at a new authoritative recovery boundary", () => {
    const queue = new DeferredTerminalOutputQueue();
    for (let generation = 0; generation <= MAX_DEFERRED_OUTPUT_RECORDS; generation += 1) {
      queue.enqueue({ data: ownTerminalBytes(new Uint8Array()), generation });
    }
    queue.reset();
    expect(queue.enqueue({ data: ownTerminalBytes(new Uint8Array([1])), generation: 1 })).toBe("accepted");
    expect(queue.drain()).toHaveLength(1);
  });
});
