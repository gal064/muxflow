import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalWriteScheduler, WRITE_FLUSH_FALLBACK_MS } from "./TerminalWriteScheduler";
import { ownTerminalBytes } from "./TerminalBytes";

interface HarnessOptions {
  maxBytesPerFrame?: number;
  maxPendingBytes?: number;
  maxPendingRecords?: number;
}

/** The renderer-shaped seams: a recording xterm, manual frames, observed accounting. */
function harness(options: HarnessOptions = {}) {
  const frames: FrameRequestCallback[] = [];
  const written: number[][] = [];
  const completions: Array<() => void> = [];
  const pending: number[] = [];
  const overflow: Array<[number, number | undefined]> = [];
  const scheduler = new TerminalWriteScheduler(
    (chunk, done) => { written.push(Array.from(chunk)); completions.push(done); },
    (callback) => { frames.push(callback); return frames.length; },
    () => undefined,
    options.maxBytesPerFrame ?? 64,
    options.maxPendingBytes ?? 1024,
    (bytes) => pending.push(bytes),
    (bytes, records) => overflow.push([bytes, records]),
    undefined,
    options.maxPendingRecords ?? 4096,
  );
  return { scheduler, frames, written, completions, pending, overflow };
}

/** Acknowledges completions and fires frames until the scheduler goes quiet. */
function drain(h: ReturnType<typeof harness>): void {
  let ticks = 0;
  while ((h.completions.length > 0 || h.frames.length > 0) && ticks < 100) {
    while (h.completions.length > 0) h.completions.shift()!();
    for (const frame of h.frames.splice(0, h.frames.length)) frame(ticks * 16);
    ticks += 1;
  }
}

describe("TerminalWriteScheduler", () => {
  it("drains records and their callbacks in enqueue order, including empty barriers", () => {
    const h = harness({ maxBytesPerFrame: 3 });
    const order: string[] = [];
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4, 5), () => order.push("first"))).toBe(true);
    expect(h.scheduler.enqueue(new Uint8Array(), () => order.push("barrier"))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(6), () => order.push("last"))).toBe(true);
    // The idle fast path cut the first record at the frame budget.
    expect(h.written).toEqual([[1, 2, 3]]);
    expect(order).toEqual([]);
    h.completions.shift()!();
    // The next frame carries the split remainder, the empty barrier, and the
    // record behind it, still strictly in byte order.
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(16));
    expect(h.written).toEqual([[1, 2, 3], [4, 5, 6]]);
    expect(order).toEqual([]);
    h.completions.shift()!();
    expect(order).toEqual(["first", "barrier", "last"]);
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  it("delivers an empty record synchronously when nothing is queued or in flight", async () => {
    const h = harness();
    const rendered = vi.fn();
    expect(h.scheduler.enqueue(new Uint8Array(), rendered)).toBe(true);
    // No bytes means no xterm write and no frame: the barrier resolves inline.
    expect(rendered).toHaveBeenCalledOnce();
    expect(h.written).toEqual([]);
    expect(h.frames).toEqual([]);
    expect(h.scheduler.pendingBytes).toBe(0);
    await expect(h.scheduler.sealAndDrain()).resolves.toBeUndefined();
  });

  it("admits a record that exactly fills the byte cap and latches overflow on the next byte", () => {
    const h = harness({ maxPendingBytes: 8 });
    expect(h.scheduler.enqueue(new Uint8Array(8))).toBe(true);
    // The full record is in flight; its backing still counts against the cap.
    expect(h.scheduler.enqueue(Uint8Array.of(9))).toBe(false);
    expect(h.overflow).toEqual([[9, 2]]);
    expect(h.scheduler.overflowed).toBe(true);
    h.completions.shift()!();
    // Draining in-flight bytes does not lift the latch; only a seed does.
    expect(h.scheduler.enqueue(Uint8Array.of(9))).toBe(false);
    expect(h.overflow).toHaveLength(1);
    h.scheduler.replace(Uint8Array.of(7));
    expect(h.scheduler.overflowed).toBe(false);
    drain(h);
    expect(h.written.flat()).toEqual([...new Array(8).fill(0), 0x1b, 0x63, 7]);
  });

  it("admits exactly maxPendingRecords records and drops the queue on the record after", () => {
    const h = harness({ maxPendingRecords: 3 });
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(2))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(3))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(4))).toBe(false);
    expect(h.overflow).toEqual([[4, 4]]);
    // Only the byte already inside xterm's parser survives the drop.
    expect(h.scheduler.pendingBytes).toBe(1);
    h.completions.shift()!();
    expect(h.scheduler.pendingBytes).toBe(0);
    expect(h.written).toEqual([[1]]);
  });

  it("reconstructs a partial in-flight record's backing when the queue is cleared", () => {
    const h = harness({ maxBytesPerFrame: 2, maxPendingBytes: 10 });
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4, 5, 6))).toBe(true);
    // Two bytes are in flight; the whole six-byte backing is still queued.
    expect(h.written).toEqual([[1, 2]]);
    expect(h.scheduler.pendingBytes).toBe(6);
    expect(h.scheduler.retainedQueueByteLength).toBe(6);
    h.scheduler.clear();
    // The queue slot is gone but the in-flight write still references the full
    // backing allocation, so admission must keep charging for all six bytes.
    expect(h.scheduler.pendingBytes).toBe(2);
    expect(h.scheduler.retainedQueueByteLength).toBe(0);
    expect(h.scheduler.enqueue(new Uint8Array([7, 8, 9, 10]))).toBe(true);
    drain(h);
    // Bytes 3..6 were dropped by clear and must never reach xterm.
    expect(h.written.flat()).toEqual([1, 2, 7, 8, 9, 10]);
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  it("refuses admission past the reconstructed in-flight backing after a clear", () => {
    const h = harness({ maxBytesPerFrame: 2, maxPendingBytes: 10 });
    expect(h.scheduler.enqueue(new Uint8Array(6))).toBe(true);
    h.scheduler.clear();
    // 6 reconstructed in-flight backing bytes + 5 new ones exceed the cap of 10.
    expect(h.scheduler.enqueue(new Uint8Array(5))).toBe(false);
    expect(h.overflow).toEqual([[11, 1]]);
    expect(h.scheduler.overflowed).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(false);
    expect(h.overflow).toHaveLength(1);
  });

  it("charges a replace seed against a partial in-flight record's reconstructed backing", () => {
    const h = harness({ maxBytesPerFrame: 2, maxPendingBytes: 10 });
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4, 5, 6))).toBe(true);
    // Seed of 2 bytes + the 2-byte reset preamble lands exactly on the cap
    // together with the 6 reconstructed in-flight backing bytes.
    h.scheduler.replace(Uint8Array.of(7, 8));
    drain(h);
    expect(h.written.flat()).toEqual([1, 2, 0x1b, 0x63, 7, 8]);
    expect(h.scheduler.pendingBytes).toBe(0);
    expect(h.overflow).toEqual([]);
  });

  it("counts an owned subarray's full backing buffer against the cap, not its view length", () => {
    const h = harness({ maxPendingBytes: 17 });
    expect(h.scheduler.enqueue(Uint8Array.of(9))).toBe(true);
    const backing = new ArrayBuffer(16);
    const view = ownTerminalBytes(new Uint8Array(backing, 4, 4));
    // 1 in-flight backing byte + the full 16-byte allocation is exactly 17.
    expect(h.scheduler.enqueueOwned(view)).toBe(true);
    expect(h.scheduler.pendingBytes).toBe(5);
    expect(h.scheduler.retainedQueueByteLength).toBe(16);
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(false);
    expect(h.overflow).toEqual([[18, 3]]);
  });

  it("writes a record exactly matching the frame budget whole and defers the next record", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4))).toBe(true);
    // An exact-budget record is one write, not a split.
    expect(h.written).toEqual([[1, 2, 3, 4]]);
    expect(h.scheduler.enqueue(Uint8Array.of(5, 6))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(7, 8, 9))).toBe(true);
    h.completions.shift()!();
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(16));
    // A full record behind earlier pieces never splits mid-frame: including a
    // partial after other pieces would double-retain its backing allocation.
    expect(h.written).toEqual([[1, 2, 3, 4], [5, 6]]);
    h.completions.shift()!();
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(32));
    expect(h.written).toEqual([[1, 2, 3, 4], [5, 6], [7, 8, 9]]);
    h.completions.shift()!();
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  /**
   * What a history splice does, and why it stopped being a refusal.
   *
   * The rewrite is composed inside a barrier's callback, from a serialization of
   * what xterm has finished with. The records behind that barrier are not in it
   * and have not been applied, so dropping them with the rest of the queue would
   * lose output — which is what made every page for a pane that never stops
   * printing a refusal instead. Kept and re-queued behind the rewrite.
   */
  it("keeps the records queued behind a barrier when a replace rewrites the buffer", () => {
    const h = harness();
    const order: string[] = [];
    expect(h.scheduler.enqueue(Uint8Array.of(1), () => order.push("first"))).toBe(true);
    expect(h.scheduler.enqueue(new Uint8Array(), () => {
      order.push("barrier");
      expect(h.scheduler.replace(Uint8Array.of(7), false, () => order.push("rewrite"), true)).toBe(true);
    })).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(2), () => order.push("behind-a"))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(3), () => order.push("behind-b"))).toBe(true);
    // The first record is in flight, so the barrier is still queued behind it
    // along with the two writes that arrived after it.
    expect(h.written).toEqual([[1]]);

    h.completions.shift()!();
    expect(order).toEqual(["first", "barrier"]);
    drain(h);

    // The reset and its payload first, then the retained records in the order
    // they were given — never their bytes twice, and never a byte short.
    expect(h.written).toEqual([[1], [0x1b, 0x63, 7, 2, 3]]);
    expect(order).toEqual(["first", "barrier", "rewrite", "behind-a", "behind-b"]);
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  /**
   * The one record that cannot be put back. Its first half is already on the
   * terminal, the reset would erase it, and the second half alone is a
   * half-parsed sequence — so the rewrite is refused and the queue is left
   * exactly as it was. `#flush` never splits a record ahead of a barrier, so
   * this is the queue keeping its own guarantee rather than a case a caller
   * reaches.
   */
  it("refuses to keep a record xterm has already half-written", () => {
    const h = harness({ maxBytesPerFrame: 2 });
    const order: string[] = [];
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4), () => order.push("split"))).toBe(true);
    expect(h.written).toEqual([[1, 2]]);

    expect(h.scheduler.replace(Uint8Array.of(7), true, undefined, true)).toBe(false);

    h.completions.shift()!();
    drain(h);
    expect(h.written).toEqual([[1, 2], [3, 4]]);
    expect(order).toEqual(["split"]);
  });

  it("keeps pending bytes equal to enqueued minus acknowledged under interleaving", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    const input: number[] = [];
    let nextByte = 0;
    const enqueueRecord = (size: number) => {
      const record = Uint8Array.from({ length: size }, () => (nextByte = (nextByte + 1) & 0xff));
      input.push(...record);
      expect(h.scheduler.enqueue(record)).toBe(true);
    };
    let acked = 0;
    let ackIndex = 0;
    const ackNext = () => {
      h.completions.shift()!();
      acked += h.written[ackIndex].length;
      ackIndex += 1;
      expect(h.scheduler.pendingBytes).toBe(input.length - acked);
    };
    enqueueRecord(1);
    enqueueRecord(7);
    enqueueRecord(0);
    enqueueRecord(3);
    expect(h.scheduler.pendingBytes).toBe(11);
    ackNext();
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(16));
    // Issuing a write moves bytes in flight without shrinking the backlog.
    expect(h.scheduler.pendingBytes).toBe(input.length - acked);
    enqueueRecord(5);
    ackNext();
    enqueueRecord(2);
    drain(h);
    while (ackIndex < h.written.length) {
      acked += h.written[ackIndex].length;
      ackIndex += 1;
    }
    expect(acked).toBe(input.length);
    expect(h.scheduler.pendingBytes).toBe(0);
    expect(h.written.flat()).toEqual(input);
    // Every published backlog observation was a real, non-negative byte count.
    expect(h.pending.every((bytes) => bytes >= 0)).toBe(true);
    expect(h.pending.at(-1)).toBe(0);
  });
});

/**
 * The harness's frame seam only ever records callbacks, so a test that never
 * fires them is a window macOS has stopped painting: timers still run, frames
 * never come. Everything here is about the queue staying alive through that.
 */
describe("TerminalWriteScheduler without a frame clock", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("flushes on the fallback timer when no frame ever arrives", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    const rendered = vi.fn();
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    // The idle fast path took the first record; the latch now sends everything
    // behind it through the frame path, which is the path that has stopped.
    expect(h.written).toEqual([[1]]);
    h.completions.shift()!();
    expect(h.scheduler.enqueue(Uint8Array.of(2, 3), rendered)).toBe(true);
    expect(h.written).toEqual([[1]]);

    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS - 1);
    expect(h.written).toEqual([[1]]);
    vi.advanceTimersByTime(1);
    expect(h.written).toEqual([[1], [2, 3]]);
    // And the completion protocol the reveal latch hangs off still runs.
    h.completions.shift()!();
    expect(rendered).toHaveBeenCalledOnce();
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  it("keeps draining record after record on the fallback alone", () => {
    const h = harness({ maxBytesPerFrame: 2 });
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    h.completions.shift()!();
    expect(h.scheduler.enqueue(Uint8Array.of(2, 3))).toBe(true);
    expect(h.scheduler.enqueue(Uint8Array.of(4, 5))).toBe(true);
    for (let tick = 0; tick < 4; tick++) {
      vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS);
      while (h.completions.length > 0) h.completions.shift()!();
    }
    expect(h.written.flat()).toEqual([1, 2, 3, 4, 5]);
    expect(h.scheduler.pendingBytes).toBe(0);
    expect(h.frames.length).toBeGreaterThan(0);
  });

  it("releases the once-per-frame immediate write on the fallback too", () => {
    const h = harness();
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    expect(h.written).toEqual([[1]]);
    h.completions.shift()!();
    // Without the fallback this latch is held for the whole occlusion, and the
    // first record of a freshly created pane waits behind it.
    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS);
    expect(h.scheduler.enqueue(Uint8Array.of(2))).toBe(true);
    expect(h.written).toEqual([[1], [2]]);
  });

  it("defers both the frame and the fallback behind an unacknowledged write", () => {
    const h = harness({ maxBytesPerFrame: 2 });
    expect(h.scheduler.enqueue(Uint8Array.of(1, 2, 3, 4))).toBe(true);
    expect(h.written).toEqual([[1, 2]]);
    // xterm still owns the first chunk. The fallback substitutes for a missing
    // frame tick; it does not get to overtake the parser.
    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS * 3);
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(16));
    expect(h.written).toEqual([[1, 2]]);
    h.completions.shift()!();
    expect(h.written).toEqual([[1, 2], [3, 4]]);
  });

  it("cancels the fallback when a frame wins the race, and never flushes twice", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    h.completions.shift()!();
    expect(h.scheduler.enqueue(Uint8Array.of(2, 3))).toBe(true);
    h.frames.splice(0, h.frames.length).forEach((frame) => frame(16));
    expect(h.written).toEqual([[1], [2, 3]]);
    h.completions.shift()!();
    // The timer the frame beat is gone, not merely held off by backpressure: an
    // idle queue plus three fallback windows still produces no second write.
    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS * 3);
    expect(h.written).toEqual([[1], [2, 3]]);
    expect(h.scheduler.pendingBytes).toBe(0);
  });

  it("drops its pending fallback when the queue is cleared", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    h.completions.shift()!();
    expect(h.scheduler.enqueue(Uint8Array.of(2, 3))).toBe(true);
    h.scheduler.clear();
    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS * 2);
    // Dropped bytes stay dropped: a timer cannot resurrect a cleared queue.
    expect(h.written).toEqual([[1]]);
  });

  it("drops its pending fallback when the scheduler is disposed", () => {
    const h = harness({ maxBytesPerFrame: 4 });
    expect(h.scheduler.enqueue(Uint8Array.of(1))).toBe(true);
    h.completions.shift()!();
    expect(h.scheduler.enqueue(Uint8Array.of(2, 3))).toBe(true);
    h.scheduler.dispose();
    vi.advanceTimersByTime(WRITE_FLUSH_FALLBACK_MS * 2);
    // No write into a terminal that has already been torn down.
    expect(h.written).toEqual([[1]]);
  });
});
