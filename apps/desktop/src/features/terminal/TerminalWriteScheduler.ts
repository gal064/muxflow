import type { OperationRecorder } from "../../perf/operations";
import { copyTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";

type FrameRequest = (callback: FrameRequestCallback) => number;
type FrameCancel = (handle: number) => void;
interface QueuedWrite {
  bytes: Uint8Array;
  backingByteLength: number;
  onRendered?: () => void;
}

function joinChunks(pieces: Uint8Array[], length: number): Uint8Array {
  if (pieces.length === 1) return pieces[0];
  const joined = new Uint8Array(length);
  let offset = 0;
  for (const piece of pieces) {
    joined.set(piece, offset);
    offset += piece.byteLength;
  }
  return joined;
}

/** A byte-preserving queue bounded across both JS and xterm's async parser. */
export class TerminalWriteScheduler {
  readonly #queue: Array<QueuedWrite | undefined> = [];
  #queueHead = 0;
  #frame?: number;
  #disposed = false;
  #pendingBytes = 0;
  #inFlightBytes = 0;
  #queuedBackingBytes = 0;
  #inFlightBackingBytes = 0;
  #overflowed = false;
  #accepting = true;
  #immediateWriteUsed = false;
  #immediateResetFrame?: number;
  readonly #drainWaiters = new Set<() => void>();

  constructor(
    readonly writeChunk: (chunk: Uint8Array, done: () => void) => void,
    readonly requestFrame: FrameRequest = (callback) => window.requestAnimationFrame(callback),
    readonly cancelFrame: FrameCancel = (handle) => window.cancelAnimationFrame(handle),
    readonly maxBytesPerFrame = 256 * 1024,
    readonly maxPendingBytes = 8 * 1024 * 1024,
    readonly onPendingBytes?: (bytes: number) => void,
    readonly onOverflow?: (pendingBytes: number) => void,
    readonly measurements?: OperationRecorder,
  ) {}

  /** Copies borrowed caller data once before it can outlive the call. */
  enqueue(bytes: Uint8Array, onRendered?: () => void): boolean {
    if (bytes.byteLength === 0) return this.#acceptEmpty(onRendered);
    if (!this.#admit(bytes.byteLength)) return false;
    const owned = copyTerminalBytes(bytes);
    this.measurements?.add("terminal.scheduler.copiedBytes", bytes.byteLength);
    this.#commit(owned, onRendered);
    return true;
  }

  /** Transfers an already-exclusive buffer without another ownership copy. */
  enqueueOwned(bytes: OwnedTerminalBytes, onRendered?: () => void): boolean {
    if (bytes.byteLength === 0) return this.#acceptEmpty(onRendered);
    if (!this.#admit(bytes.buffer.byteLength)) return false;
    this.#commit(bytes, onRendered);
    return true;
  }

  replace(bytes: Uint8Array, recoverOverflow = true, onRendered?: () => void): void {
    if (this.#disposed || (this.#overflowed && !recoverOverflow)) return;
    this.#dropQueued();
    this.#overflowed = false;
    const length = bytes.byteLength + 2;
    if (!this.#admit(length)) return;
    const resetAndBytes = new Uint8Array(length);
    resetAndBytes.set([0x1b, 0x63]);
    resetAndBytes.set(bytes, 2);
    this.measurements?.add("terminal.scheduler.copiedBytes", bytes.byteLength);
    this.#commit(resetAndBytes, onRendered);
  }

  clear(): void {
    this.#dropQueued();
    this.#overflowed = false;
  }

  dispose(): void {
    this.clear();
    this.#disposed = true;
    this.#accepting = false;
    if (this.#immediateResetFrame !== undefined) this.cancelFrame(this.#immediateResetFrame);
    this.#immediateResetFrame = undefined;
    this.#pendingBytes = 0;
    this.#inFlightBytes = 0;
    this.#queuedBackingBytes = 0;
    this.#inFlightBackingBytes = 0;
    this.#resolveDrainWaiters();
  }

  /** Stop admitting writes and resolve only after queued and in-flight bytes reach xterm. */
  sealAndDrain(): Promise<void> {
    this.#accepting = false;
    if (this.#pendingBytes === 0) return Promise.resolve();
    return new Promise((resolve) => this.#drainWaiters.add(resolve));
  }

  get pendingBytes(): number {
    return this.#pendingBytes;
  }

  get overflowed(): boolean {
    return this.#overflowed;
  }

  /** Live backing bytes still strongly referenced by queue slots. */
  get retainedQueueByteLength(): number {
    return this.#queuedBackingBytes;
  }

  #acceptEmpty(onRendered?: () => void): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    if (onRendered) this.#notifyRendered([onRendered]);
    return true;
  }

  #admit(backingByteLength: number): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    const retainedBytes = this.#queuedBackingBytes + this.#inFlightBackingBytes;
    if (retainedBytes + backingByteLength <= this.maxPendingBytes) return true;
    const attemptedBytes = retainedBytes + backingByteLength;
    this.#dropQueued();
    this.#overflowed = true;
    this.#notifyOverflow(attemptedBytes);
    return false;
  }

  #commit(bytes: Uint8Array, onRendered?: () => void): void {
    this.measurements?.add("terminal.scheduler.enqueueOperations");
    this.measurements?.add("terminal.scheduler.inputBytes", bytes.byteLength);
    if (onRendered) this.measurements?.add("terminal.scheduler.callbacksQueued");
    const backingByteLength = bytes.buffer.byteLength;
    this.#queue.push({ bytes, backingByteLength, onRendered });
    this.#pendingBytes += bytes.byteLength;
    this.#queuedBackingBytes += backingByteLength;
    this.measurements?.highWater?.("terminal.scheduler.pendingBytes", this.#pendingBytes);
    this.measurements?.highWater?.("terminal.scheduler.queueDepth", this.#queueLength());
    this.#notifyPendingBytes();
    this.#schedule();
  }

  #dropQueued(): void {
    if (this.#inFlightBytes > 0 && this.#inFlightBackingBytes === 0) {
      this.#inFlightBackingBytes = this.#queue[this.#queueHead]?.backingByteLength ?? 0;
    }
    this.#queue.length = 0;
    this.#queueHead = 0;
    this.#queuedBackingBytes = 0;
    this.#pendingBytes = this.#inFlightBytes;
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    this.#notifyPendingBytes();
  }

  #queueLength(): number {
    return this.#queue.length - this.#queueHead;
  }

  #schedule(): void {
    if (this.#disposed || this.#inFlightBytes || this.#frame !== undefined || this.#queueLength() === 0) return;
    if (this.#queueLength() === 1 && !this.#immediateWriteUsed) {
      this.#immediateWriteUsed = true;
      this.#armImmediateWriteReset();
      this.#flush();
      return;
    }
    this.#frame = this.requestFrame(() => this.#flush());
    this.measurements?.add("terminal.scheduler.framesRequested");
  }

  #armImmediateWriteReset(): void {
    if (this.#immediateResetFrame !== undefined) return;
    this.#immediateResetFrame = this.requestFrame(() => {
      this.#immediateResetFrame = undefined;
      this.#immediateWriteUsed = false;
      this.#schedule();
    });
    this.measurements?.add("terminal.scheduler.framesRequested");
  }

  /** Coalesces at most one frame budget while keeping one xterm write in flight. */
  #flush(): void {
    this.#frame = undefined;
    if (this.#disposed || this.#inFlightBytes || this.#queueLength() === 0) return;
    this.measurements?.add("terminal.scheduler.framesFlushed");
    const pieces: Uint8Array[] = [];
    const rendered: Array<() => void> = [];
    let length = 0;
    let consumedBackingBytes = 0;
    let partialRecord = false;
    while (length < this.maxBytesPerFrame && this.#queueLength() > 0) {
      const first = this.#queue[this.#queueHead];
      if (!first) throw new Error("terminal scheduler queue invariant violated");
      const remainingBudget = this.maxBytesPerFrame - length;
      // A joined chunk duplicates its pieces. Never include a partial record
      // after earlier pieces, because its full backing must remain queued.
      if (pieces.length > 0 && first.bytes.byteLength > remainingBudget) break;
      const take = Math.min(first.bytes.byteLength, remainingBudget);
      pieces.push(first.bytes.subarray(0, take));
      length += take;
      if (take === first.bytes.byteLength) {
        // Release byte buffers and callback closures as soon as the slot is
        // consumed. Accounting drops them after xterm completes; keeping dead
        // slots until compaction would let actual retention exceed the cap.
        this.#queue[this.#queueHead] = undefined;
        this.#queueHead += 1;
        this.#queuedBackingBytes -= first.backingByteLength;
        consumedBackingBytes += first.backingByteLength;
        this.measurements?.add("terminal.scheduler.dequeueOperations");
        if (first.onRendered) rendered.push(first.onRendered);
      } else {
        first.bytes = first.bytes.subarray(take);
        partialRecord = true;
      }
    }
    this.#compactQueue();
    const chunk = joinChunks(pieces, length);
    if (pieces.length > 1) this.measurements?.add("terminal.scheduler.copiedBytes", length);
    this.#inFlightBytes = length;
    this.#inFlightBackingBytes = pieces.length > 1
      ? chunk.buffer.byteLength
      : partialRecord ? 0 : consumedBackingBytes;
    let completed = false;
    const settle = (succeeded: boolean) => {
      if (completed) return;
      completed = true;
      this.#pendingBytes -= this.#inFlightBytes;
      this.#inFlightBytes = 0;
      this.#inFlightBackingBytes = 0;
      this.#notifyPendingBytes();
      if (succeeded) {
        this.#notifyRendered(rendered);
        this.measurements?.add("terminal.scheduler.callbacksInvoked", rendered.length);
      }
      this.#resolveDrainWaiters();
      if (succeeded) this.#schedule();
    };
    const done = () => settle(true);
    try {
      this.measurements?.add("terminal.scheduler.xtermWrites");
      this.measurements?.add("terminal.scheduler.xtermWriteBytes", chunk.byteLength);
      this.writeChunk(chunk, done);
    } catch {
      // A synchronous completion means xterm accepted and finished the write;
      // a later throw cannot retroactively turn it into lost output.
      if (completed) return;
      const failedPendingBytes = this.#pendingBytes;
      settle(false);
      this.#dropQueued();
      this.#overflowed = true;
      this.#resolveDrainWaiters();
      this.#notifyOverflow(failedPendingBytes);
    }
  }

  #compactQueue(): void {
    if (this.#queueHead === 0) return;
    if (this.#queueHead === this.#queue.length) {
      this.#queue.length = 0;
      this.#queueHead = 0;
      return;
    }
    if (this.#queueHead < 64 || this.#queueHead * 2 < this.#queue.length) return;
    const movedSlots = this.#queue.length - this.#queueHead;
    this.#queue.copyWithin(0, this.#queueHead);
    this.#queue.length -= this.#queueHead;
    this.#queueHead = 0;
    this.measurements?.add("terminal.scheduler.queueCompactions");
    this.measurements?.add("terminal.scheduler.queueSlotsMoved", movedSlots);
  }

  #resolveDrainWaiters(): void {
    if (this.#pendingBytes !== 0) return;
    for (const resolve of this.#drainWaiters) resolve();
    this.#drainWaiters.clear();
  }

  #notifyPendingBytes(): void {
    if (!this.onPendingBytes) return;
    try {
      this.onPendingBytes(this.#pendingBytes);
    } catch {
      this.measurements?.add("terminal.scheduler.observerErrors");
    }
  }

  #notifyOverflow(pendingBytes: number): void {
    if (!this.onOverflow) return;
    try {
      this.onOverflow(pendingBytes);
    } catch {
      this.measurements?.add("terminal.scheduler.observerErrors");
    }
  }

  #notifyRendered(callbacks: Array<() => void>): void {
    for (const callback of callbacks) {
      try {
        callback();
      } catch {
        // One pane notification must not strand the queue or prevent sibling
        // callbacks from observing bytes xterm successfully applied.
        this.measurements?.add("terminal.scheduler.observerErrors");
      }
    }
  }
}
