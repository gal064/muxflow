import { describe, expect, it } from "vitest";
import { TerminalStateCache } from "./TerminalStateCache";

describe("TerminalStateCache", () => {
  it("keeps only bounded, recently used hidden pane snapshots", () => {
    const cache = new TerminalStateCache(2, 10);
    cache.set("%1", "one"); cache.set("%2", "two");
    expect(cache.get("%1")?.serialized).toBe("one");
    cache.set("%3", "three");
    expect(cache.get("%2")).toBeUndefined();
    expect(cache.size).toBe(2);
  });

  it("rejects oversized serialization rather than retaining unbounded memory", () => {
    const cache = new TerminalStateCache(2, 3);
    cache.set("%1", "long");
    expect(cache.size).toBe(0);
  });

  it("enforces a UTF-8 aggregate LRU budget across many pane snapshots", () => {
    const cache = new TerminalStateCache(100, 12, 20);
    for (let pane = 1; pane <= 50; pane += 1) cache.set(`%${pane}`, "λλ");
    expect(cache.size).toBe(5);
    expect(cache.retainedByteLength).toBe(20);
    expect(cache.get("%1")).toBeUndefined();
    expect(cache.get("%50")?.serialized).toBe("λλ");
  });

  it("measures the screen in UTF-8 bytes rather than characters", () => {
    const cache = new TerminalStateCache(2, 10, 10);
    cache.set("%1", "λλ", { terminalEpoch: 7, outputGeneration: 9 });
    expect(cache.get("%1")).toMatchObject({ byteLength: 4, terminalEpoch: 7, outputGeneration: 9 });
    expect(cache.retainedByteLength).toBe(4);
  });

  // This cache is now the only copy of a hidden pane's screen, so declining one
  // has to drop what it was holding rather than keep a stale screen beside a
  // newer one it refused. The pane's next reveal is answered with a seed.
  it("forgets the screen it was holding when a newer one is refused", () => {
    const cache = new TerminalStateCache(2, 10, 10);
    cache.set("%1", "old");
    cache.set("%1", "a screen past the budget");
    expect(cache.get("%1")).toBeUndefined();
    expect(cache.retainedByteLength).toBe(0);
  });
});
