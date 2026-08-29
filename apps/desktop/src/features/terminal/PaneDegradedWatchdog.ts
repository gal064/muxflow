/**
 * Bounded retries for a pane that is stuck in a degraded state.
 *
 * Several per-pane degraded states are one-shot latches: the hub sets
 * `awaitingSeed` and drops everything until a seed arrives, the hub asks for one
 * conflict reseed, the renderer asks for one resnapshot after an overflow, the
 * deferred-output queue blocks itself, a reveal that failed clears its latch and
 * nothing re-runs it. Each of those has exactly one recovery signal and no time
 * bound, so if that signal is lost — the host suppressed the seed, the event was
 * dropped — the pane is frozen until it is remounted. That is the "one terminal
 * pane frozen forever" class of bug.
 *
 * This is the missing time bound. It holds the reasons a pane is currently
 * believed to be degraded; while that set is non-empty a single timer runs, and
 * each expiry asks the owner to retry recovery. Retries back off so a host that
 * genuinely cannot answer is not hammered, and the whole thing disarms the
 * moment the pane proves it is healthy — content applied, or output flowing.
 *
 * Deliberately timer-per-episode rather than a poll: a healthy pane arms
 * nothing, and `noteHealthy` on the output hot path is a single set-size check.
 */

export type PaneDegradedReason =
  /** The hub is holding this pane's output back until a seed arrives. */
  | "hubAwaitingSeed"
  /** The hub rejected a stale or conflicting handoff and asked for a reseed. */
  | "hubConflictReseed"
  /** Pane recovery cleared the screen and is waiting for an authoritative seed. */
  | "paneAwaitingSeed"
  /** The renderer refused content and asked the pane to re-seed it. */
  | "rendererReseed"
  /** Output outran the deferred-recovery queue, which is now blocking. */
  | "deferredOverflow"
  /** Marking this pane visible failed, so the host may still believe it is hidden. */
  | "revealFailed"
  /** This watchdog asked for a seed and has not seen one land yet. */
  | "seedRequested";

/** First retry after this long continuously degraded. */
export const PANE_WATCHDOG_BASE_DELAY_MS = 2_000;
/** Retries double up to here and then repeat at this cadence. */
export const PANE_WATCHDOG_MAX_DELAY_MS = 30_000;

/** Most actionable first: what the user is told about an episode. */
const REASON_PRIORITY: readonly PaneDegradedReason[] = [
  "revealFailed",
  "deferredOverflow",
  "rendererReseed",
  "hubConflictReseed",
  "hubAwaitingSeed",
  "paneAwaitingSeed",
  "seedRequested",
];

const REASON_DESCRIPTIONS: Record<PaneDegradedReason, string> = {
  revealFailed: "This pane is waiting for the host to show it",
  deferredOverflow: "Output outran this pane's recovery buffer",
  rendererReseed: "The terminal renderer refused this pane's content",
  hubConflictReseed: "A conflicting terminal handoff was rejected",
  hubAwaitingSeed: "This pane's output is held back until a seed arrives",
  paneAwaitingSeed: "This pane is waiting for a fresh terminal seed",
  seedRequested: "This pane is still waiting for the terminal seed it asked for",
};

export function describePaneDegradation(reason: PaneDegradedReason): string {
  return REASON_DESCRIPTIONS[reason];
}

export interface PaneWatchdogTimers {
  setTimer(run: () => void, milliseconds: number): unknown;
  clearTimer(handle: unknown): void;
}

const realTimers: PaneWatchdogTimers = {
  setTimer: (run, milliseconds) => setTimeout(run, milliseconds),
  clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/**
 * `attempt` is zero-based within one degraded episode, so the owner can surface
 * the episode exactly once and count the rest silently.
 */
export type PaneWatchdogRetry = (reason: PaneDegradedReason, attempt: number) => void;

export class PaneDegradedWatchdog {
  readonly #reasons = new Set<PaneDegradedReason>();
  readonly #timers: PaneWatchdogTimers;
  readonly #retry: PaneWatchdogRetry;
  #attempts = 0;
  #timer: unknown;
  #stopped = false;

  constructor(retry: PaneWatchdogRetry, timers: PaneWatchdogTimers = realTimers) {
    this.#retry = retry;
    this.#timers = timers;
  }

  /** Records a degraded state and arms the bound if this is a new episode. */
  note(reason: PaneDegradedReason): void {
    if (this.#stopped || this.#reasons.has(reason)) return;
    this.#reasons.add(reason);
    this.#arm();
  }

  /** Withdraws one reason. The bound survives while any other reason stands. */
  clear(reason: PaneDegradedReason): void {
    if (!this.#reasons.delete(reason)) return;
    if (this.#reasons.size === 0) this.#recovered();
  }

  /**
   * The pane proved it is working — content applied, or output flowed. Every
   * reason is withdrawn and the backoff starts over, so an intermittent fault
   * gets a fast first retry rather than inheriting the last episode's cadence.
   */
  noteHealthy(): void {
    if (this.#reasons.size === 0 && this.#attempts === 0) return;
    this.#reasons.clear();
    this.#recovered();
  }

  /** Permanent: this pane instance is gone and must never retry again. */
  stop(): void {
    this.#stopped = true;
    this.#reasons.clear();
    this.#disarm();
  }

  get degraded(): boolean {
    return this.#reasons.size > 0;
  }

  /** The reason an episode is reported under, or `undefined` when healthy. */
  get primaryReason(): PaneDegradedReason | undefined {
    for (const reason of REASON_PRIORITY) {
      if (this.#reasons.has(reason)) return reason;
    }
    return undefined;
  }

  get attempts(): number {
    return this.#attempts;
  }

  #recovered(): void {
    this.#attempts = 0;
    this.#disarm();
  }

  #arm(): void {
    if (this.#stopped || this.#timer !== undefined) return;
    const delay = Math.min(
      PANE_WATCHDOG_BASE_DELAY_MS * 2 ** this.#attempts,
      PANE_WATCHDOG_MAX_DELAY_MS,
    );
    this.#timer = this.#timers.setTimer(() => {
      this.#timer = undefined;
      this.#fire();
    }, delay);
  }

  #disarm(): void {
    if (this.#timer === undefined) return;
    this.#timers.clearTimer(this.#timer);
    this.#timer = undefined;
  }

  #fire(): void {
    const reason = this.primaryReason;
    if (this.#stopped || reason === undefined) return;
    const attempt = this.#attempts;
    this.#attempts += 1;
    // Before the callback, not after: the retry asks for a seed, and until that
    // seed lands the pane is still degraded even if every other reason is
    // withdrawn as a side effect of asking.
    this.#reasons.add("seedRequested");
    try {
      this.#retry(reason, attempt);
    } finally {
      // A retry that throws is still a retry: re-arm regardless so one bad
      // recovery attempt cannot be the thing that freezes the pane.
      if (!this.#stopped && this.#reasons.size > 0) this.#arm();
    }
  }
}
