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
});
