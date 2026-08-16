import { describe, expect, it } from "vitest";
import { TerminalStateCache } from "./TerminalStateCache";
import { prepareTerminalSnapshot } from "./api";

const prepared = (serialized: string, maxBytes = Number.MAX_SAFE_INTEGER) =>
  prepareTerminalSnapshot(serialized, maxBytes);

describe("TerminalStateCache", () => {
  it("keeps only bounded, recently used hidden pane snapshots", () => {
    const cache = new TerminalStateCache(2, 10);
    cache.set("%1", prepared("one")); cache.set("%2", prepared("two"));
    expect(cache.get("%1")?.serialized).toBe("one");
    cache.set("%3", prepared("three"));
    expect(cache.get("%2")).toBeUndefined();
    expect(cache.size).toBe(2);
  });

  it("rejects oversized serialization rather than retaining unbounded memory", () => {
    const cache = new TerminalStateCache(2, 3);
    cache.set("%1", prepared("long"));
    expect(cache.size).toBe(0);
  });

  it("enforces a UTF-8 aggregate LRU budget across many pane snapshots", () => {
    const cache = new TerminalStateCache(100, 12, 20);
    for (let pane = 1; pane <= 50; pane += 1) cache.set(`%${pane}`, prepared("λλ"));
    expect(cache.size).toBe(5);
    expect(cache.retainedByteLength).toBe(20);
    expect(cache.get("%1")).toBeUndefined();
    expect(cache.get("%50")?.serialized).toBe("λλ");
  });

  it("reuses a prepared snapshot's encoded length for exact cache accounting", () => {
    const cache = new TerminalStateCache(2, 10, 10);
    cache.set("%1", prepared("λλ"), { terminalEpoch: 7, outputGeneration: 9 });
    expect(cache.get("%1")).toMatchObject({ byteLength: 4, terminalEpoch: 7, outputGeneration: 9 });
    expect(cache.retainedByteLength).toBe(4);
  });

  it("cannot retain a snapshot rejected by the canonical encoder", () => {
    const cache = new TerminalStateCache(2, 10, 10);
    cache.set("%1", prepared("old"));
    cache.set("%1", prepared("oversized", 3));
    expect(cache.get("%1")).toBeUndefined();
    expect(cache.retainedByteLength).toBe(0);
  });
});
