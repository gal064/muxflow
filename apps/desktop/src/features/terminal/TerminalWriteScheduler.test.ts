import { describe, expect, it, vi } from "vitest";
import { TerminalWriteScheduler } from "./TerminalWriteScheduler";
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
