import { afterNextPaint, perfProbeEnabled, startPerfSpan } from "./probe";

/** One operation's authority to publish a lifecycle-owned paint measurement. */
export interface PaintTicket {
  readonly lifecycleGeneration: number;
  readonly surfaceGeneration: number;
  expectSurface(generation: number): void;
  afterPaint(isCurrent: (ticket: PaintTicket) => boolean, onPaint?: () => void): void;
  abandon(): void;
}

const INERT_PAINT_TICKET: PaintTicket = Object.freeze({
  lifecycleGeneration: 0,
  surfaceGeneration: 0,
  expectSurface: () => undefined,
  afterPaint: () => undefined,
  abandon: () => undefined,
});

class ActivePaintTicket implements PaintTicket {
  readonly lifecycleGeneration: number;
  #surfaceGeneration = 0;
  readonly #close: () => void;
  #active = true;

  constructor(names: readonly string[], lifecycleGeneration: number) {
    this.lifecycleGeneration = lifecycleGeneration;
    const closures = names.map(startPerfSpan);
    this.#close = () => closures.forEach((close) => close());
  }

  get surfaceGeneration(): number {
    return this.#surfaceGeneration;
  }

  expectSurface(generation: number): void {
    this.#surfaceGeneration = generation;
  }

  afterPaint(isCurrent: (ticket: PaintTicket) => boolean, onPaint?: () => void): void {
    afterNextPaint(() => {
      if (!this.#active || !isCurrent(this)) return;
      this.#active = false;
      this.#close();
      onPaint?.();
    });
  }

  abandon(): void {
    this.#active = false;
  }
}

/**
 * The disabled path returns one allocation-free singleton. Callers may keep
 * lifecycle logic unconditional without adding per-event objects or closures
 * to ordinary, unmeasured filesystem and editor traffic.
 */
export function createPaintTicket(names: readonly string[], lifecycleGeneration: number): PaintTicket {
  return perfProbeEnabled() ? new ActivePaintTicket(names, lifecycleGeneration) : INERT_PAINT_TICKET;
}
