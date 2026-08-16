export type FrameRequest = (callback: FrameRequestCallback) => number;
export type FrameCancel = (handle: number) => void;

/**
 * Publishes pointer geometry at most once per animation frame, then persists
 * exactly one final value. A cancelled pointer gesture commits its last valid
 * geometry just like pointer-up.
 */
export class TransientDrag<T> {
  #closed = false;
  #frame: number | undefined;
  #latest: T;

  constructor(
    initial: T,
    private readonly publish: (value: T) => void,
    private readonly persist: (value: T) => void,
    private readonly requestFrame: FrameRequest = window.requestAnimationFrame.bind(window),
    private readonly cancelFrame: FrameCancel = window.cancelAnimationFrame.bind(window),
  ) {
    this.#latest = initial;
  }

  preview(value: T): void {
    if (this.#closed) return;
    this.#latest = value;
    if (this.#frame !== undefined) return;
    this.#frame = this.requestFrame(() => {
      this.#frame = undefined;
      if (!this.#closed) this.publish(this.#latest);
    });
  }

  finish(finalValue?: T): void {
    if (this.#closed) return;
    if (finalValue !== undefined) this.#latest = finalValue;
    this.#closed = true;
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    this.publish(this.#latest);
    this.persist(this.#latest);
  }

  dispose(): void {
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    this.#closed = true;
  }
}
