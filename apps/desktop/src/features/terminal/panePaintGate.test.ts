import { afterEach, describe, expect, it, vi } from "vitest";
import {
  armPanePaint,
  awaitPanePaint,
  notePanePainted,
  PANE_PAINT_TIMEOUT_MS,
  resetPanePaintGate,
} from "./panePaintGate";

/** Resolves once the promise settles, without waiting on it. */
function settled(promise: Promise<void>): () => boolean {
  let done = false;
  void promise.then(() => { done = true; });
  return () => done;
}

afterEach(() => {
  resetPanePaintGate();
  vi.useRealTimers();
});

describe("panePaintGate", () => {
  it("holds a waiter from a pane's reveal until its screen paints", async () => {
    armPanePaint("%1");
    const waiting = settled(awaitPanePaint("%1"));
    await Promise.resolve();
    expect(waiting(), "the sidebar went ahead of the pane's own screen").toBe(false);

    notePanePainted("%1");
    await Promise.resolve();
    expect(waiting()).toBe(true);
  });

  it("releases a waiter on the timeout when the pane never paints", async () => {
    vi.useFakeTimers();
    armPanePaint("%1");
    const waiting = settled(awaitPanePaint("%1"));
    await vi.advanceTimersByTimeAsync(PANE_PAINT_TIMEOUT_MS - 1);
    expect(waiting()).toBe(false);
    // A wedged renderer, a reveal the host never answers, a pane that unmounts
    // on the way: none of them may cost the user their file tree.
    await vi.advanceTimersByTimeAsync(1);
    expect(waiting()).toBe(true);
  });

  it("waits for nothing on a pane no reveal armed", async () => {
    vi.useFakeTimers();
    // Nothing is pending for a pane this process never revealed — a pane the
    // switch did not touch, or one whose screen has been up since before the
    // caller existed. Waiting out a timeout for it would be a pause bought for
    // no ordering at all.
    const waiting = settled(awaitPanePaint("%9"));
    await Promise.resolve();
    expect(waiting()).toBe(true);
  });

  it("resolves immediately for a paint that landed before the wait", async () => {
    armPanePaint("%1");
    notePanePainted("%1");
    const waiting = settled(awaitPanePaint("%1"));
    await Promise.resolve();
    expect(waiting()).toBe(true);
  });

  it("gates again when the same pane is revealed a second time", async () => {
    armPanePaint("%1");
    notePanePainted("%1");
    // The next switch back re-reveals the pane, and its screen is owed again.
    armPanePaint("%1");
    const waiting = settled(awaitPanePaint("%1"));
    await Promise.resolve();
    expect(waiting()).toBe(false);
    notePanePainted("%1");
    await Promise.resolve();
    expect(waiting()).toBe(true);
  });

  it("releases every waiter on a pane, and only that pane", async () => {
    armPanePaint("%1");
    armPanePaint("%2");
    const first = settled(awaitPanePaint("%1"));
    const second = settled(awaitPanePaint("%1"));
    const other = settled(awaitPanePaint("%2"));
    notePanePainted("%1");
    await Promise.resolve();
    expect([first(), second(), other()]).toEqual([true, true, false]);
  });

  it("never strands a waiter on a pane the bound forgot", async () => {
    armPanePaint("%old");
    const waiting = settled(awaitPanePaint("%old"));
    // The map is bounded, so a long session's pane ids cannot accumulate. An
    // eviction says nothing about that pane's screen; leaving a promise nobody
    // can settle is the one failure this module must not have.
    for (let index = 0; index < 200; index += 1) armPanePaint(`%${index}`);
    await Promise.resolve();
    expect(waiting()).toBe(true);
  });
});
