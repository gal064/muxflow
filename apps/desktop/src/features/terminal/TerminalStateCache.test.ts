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

  // An eviction is not free: this cache is what a hidden pane's reveal resumes
  // from, so a pane evicted while the user is still working in it pays a full
  // host seed on its next reveal. Twenty was under the number of panes a real
  // session has open, which made ordinary switching evict panes that were about
  // to come back.
  it("holds a real session's worth of panes, inside a hard byte bound", () => {
    const cache = new TerminalStateCache();
    expect(cache.capacity).toBe(64);
    for (let pane = 1; pane <= 64; pane += 1) cache.set(`%${pane}`, `screen-${pane}`);
    expect(cache.size).toBe(64);
    expect(cache.get("%1")?.serialized).toBe("screen-1");

    // The count is not the only bound: the bytes are checked on every insert,
    // so a larger capacity cannot become a larger footprint.
    const bounded = new TerminalStateCache(64, 1_000, 40);
    for (let pane = 1; pane <= 64; pane += 1) bounded.set(`%${pane}`, "0123456789");
    expect(bounded.size).toBe(4);
    expect(bounded.retainedByteLength).toBe(40);
  });

  // A screen carries how far up its own history it has been paged, because the
  // pages are part of the bytes: a restore that forgot would fetch them again.
  // The size of the *next* page rides along for the same reason — the ladder
  // that grows it is sizing the cost of rewriting these bytes, and a restore
  // that forgot it would climb from the bottom again.
  it("carries a screen's paging state, defaulting to a screen nobody paged", () => {
    const cache = new TerminalStateCache();
    cache.set("%1", "plain");
    expect(cache.get("%1")).toMatchObject({
      screenSeeded: false, historyExhausted: false, historyNextPageLines: 0,
    });
    cache.set("%2", "paged", undefined, {
      screenSeeded: true, historyExhausted: true, historyNextPageLines: 2_400,
    });
    expect(cache.get("%2")).toMatchObject({
      screenSeeded: true, historyExhausted: true, historyNextPageLines: 2_400,
    });
  });

  // The invariant the docstring claims: a read is a use. `Map` iterates in
  // insertion order and eviction takes the front of it, so a `get` that did not
  // reorder would make this a FIFO — and a FIFO evicts the pane the user keeps
  // returning to, which costs that pane a full host seed on its next reveal.
  it("counts a read as a use, so the pane a reveal keeps returning to survives", () => {
    const cache = new TerminalStateCache(2, 100, 100);
    cache.set("%1", "one");
    cache.set("%2", "two");
    // %1 is the oldest by insertion and the newest by use.
    expect(cache.get("%1")?.serialized).toBe("one");
    cache.set("%3", "three");
    expect(cache.get("%1")?.serialized, "the least *recently used* screen was evicted").toBe("one");
    expect(cache.get("%2")).toBeUndefined();
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
