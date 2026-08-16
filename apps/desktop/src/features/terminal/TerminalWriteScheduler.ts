import type { OperationRecorder } from "../../perf/operations";
import { copyTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";

type FrameRequest = (callback: FrameRequestCallback) => number;
type FrameCancel = (handle: number) => void;

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
  readonly #queue: Array<{ bytes: Uint8Array; onRendered?: () => void }> = [];
  #queueHead = 0;
  #frame?: number;
  #disposed = false;
  #pendingBytes = 0;
  #inFlightBytes = 0;
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
    if (!this.#admit(bytes.byteLength)) return false;
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

  #acceptEmpty(onRendered?: () => void): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    onRendered?.();
    return true;
  }

  #admit(byteLength: number): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    if (this.#pendingBytes + byteLength <= this.maxPendingBytes) return true;
    const attemptedBytes = this.#pendingBytes + byteLength;
    this.#dropQueued();
    this.#overflowed = true;
    this.onOverflow?.(attemptedBytes);
    return false;
  }

  #commit(bytes: Uint8Array, onRendered?: () => void): void {
    this.measurements?.add("terminal.scheduler.enqueueOperations");
    this.measurements?.add("terminal.scheduler.inputBytes", bytes.byteLength);
    if (onRendered) this.measurements?.add("terminal.scheduler.callbacksQueued");
    this.#queue.push({ bytes, onRendered });
    this.#pendingBytes += bytes.byteLength;
    this.measurements?.highWater?.("terminal.scheduler.pendingBytes", this.#pendingBytes);
    this.measurements?.highWater?.("terminal.scheduler.queueDepth", this.#queueLength());
    this.onPendingBytes?.(this.#pendingBytes);
    this.#schedule();
  }

  #dropQueued(): void {
    this.#queue.length = 0;
    this.#queueHead = 0;
    this.#pendingBytes = this.#inFlightBytes;
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    this.onPendingBytes?.(this.#pendingBytes);
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
    while (length < this.maxBytesPerFrame && this.#queueLength() > 0) {
      const first = this.#queue[this.#queueHead];
      const take = Math.min(first.bytes.byteLength, this.maxBytesPerFrame - length);
      pieces.push(first.bytes.subarray(0, take));
      length += take;
      if (take === first.bytes.byteLength) {
        this.#queueHead += 1;
        this.measurements?.add("terminal.scheduler.dequeueOperations");
        if (first.onRendered) rendered.push(first.onRendered);
      } else {
        first.bytes = first.bytes.subarray(take);
      }
    }
    this.#compactQueue();
    const chunk = joinChunks(pieces, length);
    if (pieces.length > 1) this.measurements?.add("terminal.scheduler.copiedBytes", length);
    this.#inFlightBytes = length;
    let completed = false;
    const done = () => {
      if (completed) return;
      completed = true;
      this.#pendingBytes -= this.#inFlightBytes;
      this.#inFlightBytes = 0;
      this.onPendingBytes?.(this.#pendingBytes);
      for (const onRendered of rendered) onRendered();
      this.measurements?.add("terminal.scheduler.callbacksInvoked", rendered.length);
      this.#resolveDrainWaiters();
      this.#schedule();
    };
    try {
      this.measurements?.add("terminal.scheduler.xtermWrites");
      this.measurements?.add("terminal.scheduler.xtermWriteBytes", chunk.byteLength);
      this.writeChunk(chunk, done);
    } catch {
      done();
      this.#dropQueued();
      this.#overflowed = true;
      this.onOverflow?.(this.#pendingBytes);
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
    this.#queue.copyWithin(0, this.#queueHead);
    this.#queue.length -= this.#queueHead;
    this.#queueHead = 0;
    this.measurements?.add("terminal.scheduler.queueCompactions");
  }

  #resolveDrainWaiters(): void {
    if (this.#pendingBytes !== 0) return;
    for (const resolve of this.#drainWaiters) resolve();
    this.#drainWaiters.clear();
  }
}
