import type { OperationRecorder } from "../../perf/operations";
import { copyTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";

type FrameRequest = (callback: FrameRequestCallback) => number;
type FrameCancel = (handle: number) => void;
interface QueuedWrite {
  bytes: Uint8Array;
  backingByteLength: number;
  onRendered?: () => void;
  /**
   * Whether a prefix of this record has already reached xterm, leaving `bytes`
   * holding only the remainder.
   *
   * Such a record cannot be replayed after a reset: its first half is on the
   * terminal and the reset would erase it, so writing the second half alone is
   * a half-parsed escape sequence. `replace` refuses to keep one — see
   * `#queuedRecords`.
   */
  partialled?: boolean;
}

/**
 * How long a queued write may wait for a frame that may never arrive.
 *
 * The frame clock is not a liveness guarantee. macOS parks
 * `requestAnimationFrame` for a window that is occluded, minimized, or on
 * another Space, while timers and IPC keep running — so a queue that only ever
 * flushes from a frame stops flushing at all. Production journals show this
 * queue holding output for 7s, 59s, 545s and 19s with no long task and no atlas
 * churn behind it, every burst landing at once the moment the window came back.
 *
 * Parsing into a canvas nobody is painting costs almost nothing and is what
 * keeps everything hanging off the write callback current: the reveal latch, the
 * generation checkpoints, and the queue bound that would otherwise overflow and
 * force a reseed on a chatty background pane.
 *
 * 200ms is an order of magnitude past a 60Hz frame, so a live frame clock always
 * wins the race and this timer is cancelled before it can fire; and it still
 * floors an occluded pane's drain at five frame budgets a second, well past what
 * a background pane produces.
 */
export const WRITE_FLUSH_FALLBACK_MS = 200;

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
  #fallbackTimer?: ReturnType<typeof setTimeout>;
  /** Whether a flush is owed, by either the frame or the fallback timer. */
  #flushArmed = false;
  #disposed = false;
  #pendingBytes = 0;
  #inFlightBytes = 0;
  #inFlightRecords = 0;
  #queuedBackingBytes = 0;
  #inFlightBackingBytes = 0;
  #overflowed = false;
  #accepting = true;
  #immediateWriteUsed = false;
  #immediateResetFrame?: number;
  #immediateResetTimer?: ReturnType<typeof setTimeout>;
  #immediateResetArmed = false;
  readonly #drainWaiters = new Set<() => void>();

  constructor(
    readonly writeChunk: (chunk: Uint8Array, done: () => void) => void,
    readonly requestFrame: FrameRequest = (callback) => window.requestAnimationFrame(callback),
    readonly cancelFrame: FrameCancel = (handle) => window.cancelAnimationFrame(handle),
    readonly maxBytesPerFrame = 256 * 1024,
    readonly maxPendingBytes = 8 * 1024 * 1024,
    readonly onPendingBytes?: (bytes: number) => void,
    readonly onOverflow?: (pendingBytes: number, pendingRecords?: number) => void,
    readonly measurements?: OperationRecorder,
    readonly maxPendingRecords = 4096,
  ) {}

  /** Copies borrowed caller data once before it can outlive the call. */
  enqueue(bytes: Uint8Array, onRendered?: () => void): boolean {
    if (bytes.byteLength === 0) return this.#commitEmpty(onRendered);
    if (!this.#admit(bytes.byteLength)) return false;
    const owned = copyTerminalBytes(bytes);
    this.measurements?.add("terminal.scheduler.copiedBytes", bytes.byteLength);
    this.#commit(owned, onRendered);
    return true;
  }

  /** Transfers an already-exclusive buffer without another ownership copy. */
  enqueueOwned(bytes: OwnedTerminalBytes, onRendered?: () => void): boolean {
    if (bytes.byteLength === 0) return this.#commitEmpty(onRendered);
    if (!this.#admit(bytes.buffer.byteLength)) return false;
    this.#commit(bytes, onRendered);
    return true;
  }

  /**
   * Returns whether the rewrite was committed, on the same rule as `enqueue`.
   *
   * `keepQueued` is for a rewrite composed at a barrier — a history splice.
   * The writes sitting behind that barrier have not been applied, so they are
   * not in the serialization the rewrite carries, and dropping them with the
   * rest of the queue would lose output this scheduler accepted. Kept in order
   * and re-queued behind the rewrite, which is exactly where they would have
   * run: their callbacks travel with them, so nothing hanging off a write is
   * resolved early, resolved twice, or lost.
   *
   * And re-queued only once the rewrite itself is on the terminal, so the
   * rewrite gets a frame of its own. A flush coalesces up to a frame budget, so
   * committing them straight away puts the reset, the payload and the retained
   * records into one xterm write — and `onRendered` then fires on a buffer that
   * already holds the retained rows. The caller measuring what the rewrite
   * added would count those rows too and scroll the user past the lines they
   * were reading by exactly that much.
   */
  replace(bytes: Uint8Array, recoverOverflow = true, onRendered?: () => void, keepQueued = false): boolean {
    if (this.#disposed || (this.#overflowed && !recoverOverflow)) return false;
    const retained = keepQueued ? this.#queuedRecords() : [];
    if (!retained) return false;
    this.#dropQueued();
    this.#overflowed = false;
    const length = bytes.byteLength + 2;
    if (!this.#admit(length)) {
      // `#admit` refuses two ways. Past the bound it has already latched
      // overflow and told the owner, and what was retained goes with the queue
      // that overflowed — that is the overflow path doing its job, and it takes
      // a single payload past `maxPendingBytes` to reach. Sealed for a hide
      // drain it refuses silently, and dropping accepted output on the way to a
      // refusal nobody hears is how a pane loses bytes with nothing said.
      if (!this.#overflowed) this.#requeue(retained);
      return false;
    }
    const resetAndBytes = new Uint8Array(length);
    resetAndBytes.set([0x1b, 0x63]);
    resetAndBytes.set(bytes, 2);
    this.measurements?.add("terminal.scheduler.copiedBytes", bytes.byteLength);
    this.#commit(
      resetAndBytes,
      retained.length === 0 ? onRendered : () => {
        // The caller's callback first, while this buffer holds the rewrite and
        // nothing else, and the retained records after it.
        onRendered?.();
        this.#requeue(retained);
      },
    );
    return true;
  }

  /**
   * Puts records back on the queue without re-admitting them.
   *
   * They were admitted once already, and the bound governs new output rather
   * than bytes being returned to where they were.
   */
  #requeue(records: readonly QueuedWrite[]): void {
    for (const record of records) this.#commit(record.bytes, record.onRendered);
  }

  /**
   * The records still waiting, oldest first, or `undefined` when one of them
   * cannot survive a reset.
   *
   * In practice the refusal never fires from the one caller that asks: `#flush`
   * only ever splits the *first* record of a batch and that split ends the
   * batch, so a barrier is never consumed while a partialled record sits ahead
   * of it, and by the time a barrier's callback runs nothing is in flight. The
   * check is here so that the guarantee is the queue's rather than the caller's.
   */
  #queuedRecords(): QueuedWrite[] | undefined {
    const records: QueuedWrite[] = [];
    for (let index = this.#queueHead; index < this.#queue.length; index += 1) {
      const record = this.#queue[index];
      if (!record) continue;
      if (record.partialled) return undefined;
      records.push(record);
    }
    return records;
  }

  clear(): void {
    this.#dropQueued();
    this.#overflowed = false;
  }

  dispose(): void {
    this.clear();
    this.#disposed = true;
    this.#accepting = false;
    this.#cancelImmediateWriteReset();
    this.#pendingBytes = 0;
    this.#inFlightBytes = 0;
    this.#inFlightRecords = 0;
    this.#queuedBackingBytes = 0;
    this.#inFlightBackingBytes = 0;
    this.#resolveDrainWaiters();
  }

  /** Stop admitting writes and resolve only after queued and in-flight bytes reach xterm. */
  sealAndDrain(): Promise<void> {
    this.#accepting = false;
    if (this.#isDrained()) return Promise.resolve();
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

  #commitEmpty(onRendered?: () => void): boolean {
    if (!this.#admit(0)) return false;
    // Empty output is still an ordered terminal record. Queue it as a
    // zero-retention barrier so its generation callback cannot overtake bytes
    // already inside xterm's asynchronous parser.
    this.#commit(new Uint8Array(), onRendered);
    return true;
  }

  #admit(backingByteLength: number): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    const retainedBytes = this.#queuedBackingBytes + this.#inFlightBackingBytes;
    const retainedRecords = this.#queueLength() + this.#inFlightRecords;
    if (retainedBytes + backingByteLength <= this.maxPendingBytes
      && retainedRecords + 1 <= this.maxPendingRecords) return true;
    const attemptedBytes = retainedBytes + backingByteLength;
    const attemptedRecords = retainedRecords + 1;
    this.#dropQueued();
    this.#overflowed = true;
    this.#notifyOverflow(attemptedBytes, attemptedRecords);
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
    this.#cancelFlush();
    this.#notifyPendingBytes();
  }

  #queueLength(): number {
    return this.#queue.length - this.#queueHead;
  }

  #schedule(): void {
    if (this.#disposed || this.#inFlightBytes || this.#flushArmed || this.#queueLength() === 0) return;
    if (this.#flushEmptyPrefix()) {
      this.#schedule();
      return;
    }
    if (this.#queueLength() === 1 && !this.#immediateWriteUsed) {
      this.#immediateWriteUsed = true;
      this.#armImmediateWriteReset();
      this.#flush();
      return;
    }
    this.#armFlush();
  }

  /**
   * Races a frame against a wall-clock floor, whichever comes first.
   *
   * The frame stays the coalescer — it is what keeps a busy pane to one write
   * per painted frame — and the timer only substitutes for a frame clock that
   * has stopped (see `WRITE_FLUSH_FALLBACK_MS`). Both land on the same guarded
   * entry point, so the loser of the race is a no-op rather than a second flush.
   */
  #armFlush(): void {
    this.#flushArmed = true;
    this.#fallbackTimer = setTimeout(() => this.#runScheduledFlush(), WRITE_FLUSH_FALLBACK_MS);
    const frame = this.requestFrame(() => this.#runScheduledFlush());
    // A frame seam that ran the callback inline already spent this arming; the
    // handle it returned belongs to nothing and must not be left behind.
    if (this.#flushArmed) this.#frame = frame;
    else this.cancelFrame(frame);
    this.measurements?.add("terminal.scheduler.framesRequested");
  }

  #runScheduledFlush(): void {
    if (!this.#flushArmed) return;
    this.#cancelFlush();
    this.#flush();
  }

  #cancelFlush(): void {
    this.#flushArmed = false;
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    if (this.#fallbackTimer !== undefined) clearTimeout(this.#fallbackTimer);
    this.#fallbackTimer = undefined;
  }

  #flushEmptyPrefix(): boolean {
    const rendered: Array<() => void> = [];
    let consumed = 0;
    while (this.#queueLength() > 0) {
      const first = this.#queue[this.#queueHead];
      if (!first) throw new Error("terminal scheduler queue invariant violated");
      if (first.bytes.byteLength !== 0) break;
      this.#queue[this.#queueHead] = undefined;
      this.#queueHead += 1;
      consumed += 1;
      this.measurements?.add("terminal.scheduler.dequeueOperations");
      if (first.onRendered) rendered.push(first.onRendered);
    }
    if (consumed === 0) return false;
    this.#compactQueue();
    this.#notifyRendered(rendered);
    this.measurements?.add("terminal.scheduler.callbacksInvoked", rendered.length);
    this.#resolveDrainWaiters();
    return true;
  }

  /**
   * Releases the once-per-frame immediate write, on the same race as a flush.
   *
   * A latch that only a frame can clear is a latch an occluded window holds for
   * the whole occlusion, which would leave the very first queued record — the
   * one carrying a new pane's reveal — waiting on the frame clock again.
   */
  #armImmediateWriteReset(): void {
    if (this.#immediateResetArmed) return;
    this.#immediateResetArmed = true;
    this.#immediateResetTimer = setTimeout(() => this.#runImmediateWriteReset(), WRITE_FLUSH_FALLBACK_MS);
    const frame = this.requestFrame(() => this.#runImmediateWriteReset());
    if (this.#immediateResetArmed) this.#immediateResetFrame = frame;
    else this.cancelFrame(frame);
    this.measurements?.add("terminal.scheduler.framesRequested");
  }

  #runImmediateWriteReset(): void {
    if (!this.#immediateResetArmed) return;
    this.#cancelImmediateWriteReset();
    this.#immediateWriteUsed = false;
    this.#schedule();
  }

  #cancelImmediateWriteReset(): void {
    this.#immediateResetArmed = false;
    if (this.#immediateResetFrame !== undefined) this.cancelFrame(this.#immediateResetFrame);
    this.#immediateResetFrame = undefined;
    if (this.#immediateResetTimer !== undefined) clearTimeout(this.#immediateResetTimer);
    this.#immediateResetTimer = undefined;
  }

  /** Coalesces at most one frame budget while keeping one xterm write in flight. */
  #flush(): void {
    if (this.#disposed || this.#inFlightBytes || this.#queueLength() === 0) return;
    this.measurements?.add("terminal.scheduler.framesFlushed");
    const pieces: Uint8Array[] = [];
    const rendered: Array<() => void> = [];
    let length = 0;
    let consumedBackingBytes = 0;
    let consumedRecords = 0;
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
        consumedRecords += 1;
        this.measurements?.add("terminal.scheduler.dequeueOperations");
        if (first.onRendered) rendered.push(first.onRendered);
      } else {
        first.bytes = first.bytes.subarray(take);
        first.partialled = true;
        partialRecord = true;
      }
    }
    this.#compactQueue();
    if (length === 0) {
      this.#notifyRendered(rendered);
      this.measurements?.add("terminal.scheduler.callbacksInvoked", rendered.length);
      this.#resolveDrainWaiters();
      this.#schedule();
      return;
    }
    const chunk = joinChunks(pieces, length);
    if (pieces.length > 1) this.measurements?.add("terminal.scheduler.copiedBytes", length);
    this.#inFlightBytes = length;
    this.#inFlightRecords = consumedRecords;
    this.#inFlightBackingBytes = pieces.length > 1
      ? chunk.buffer.byteLength
      : partialRecord ? 0 : consumedBackingBytes;
    let completed = false;
    const settle = (succeeded: boolean) => {
      if (completed) return;
      completed = true;
      this.#pendingBytes -= this.#inFlightBytes;
      this.#inFlightBytes = 0;
      this.#inFlightRecords = 0;
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
      this.#notifyOverflow(failedPendingBytes, this.#queueLength() + this.#inFlightRecords);
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
    if (!this.#isDrained()) return;
    for (const resolve of this.#drainWaiters) resolve();
    this.#drainWaiters.clear();
  }

  #isDrained(): boolean {
    return this.#pendingBytes === 0 && this.#inFlightBytes === 0 && this.#queueLength() === 0;
  }

  #notifyPendingBytes(): void {
    if (!this.onPendingBytes) return;
    try {
      this.onPendingBytes(this.#pendingBytes);
    } catch {
      this.measurements?.add("terminal.scheduler.observerErrors");
    }
  }

  #notifyOverflow(pendingBytes: number, pendingRecords?: number): void {
    if (!this.onOverflow) return;
    try {
      this.onOverflow(pendingBytes, pendingRecords);
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
