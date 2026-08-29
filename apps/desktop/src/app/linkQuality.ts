/**
 * Whether the *network* is the thing that is wrong, said in those words.
 *
 * Shaped at 150ms RTT / 2Mbit / 1% loss, the app lost and re-established the
 * link six times in two minutes and never once told the user why: each drop
 * spoke as its own bridge failure, each reconnect cleared the strip, and the
 * only thing left on screen was a link that kept flickering. The journal had
 * every fact and the person watching had none.
 *
 * This is the missing summary. It counts the two signals the app already
 * produces — a link that dropped, and a keystroke that echoed late — and once
 * either one repeats inside its window it says the connection is the problem.
 * Nothing here probes, pings or measures: it is a pure tally over injected
 * time, so the controller owns every clock and this owns the judgement.
 */

export type LinkQualityState = "unstable" | "slow";

/**
 * Two drops this close together are a link that cannot hold, not an outage.
 * Five minutes: the shaped-link run of 2026-08-29 dropped every ~2.5 minutes
 * and was, by any standard, unstable.
 */
export const UNSTABLE_WINDOW_MS = 300_000;
export const UNSTABLE_LOSS_COUNT = 2;
/**
 * Echo lag is already thresholded by `echoLagProbe` (and deduped per pane to
 * one record per ten seconds), so a second one inside a minute is a link that
 * is answering late as a matter of course rather than a single hiccup.
 */
export const SLOW_WINDOW_MS = 60_000;
export const SLOW_LAG_COUNT = 2;
/**
 * The probe's own bar (250 ms) is a typing-feel bar, and a busy pane on a
 * healthy link clears it now and then (485 ms on a 50 ms-RTT link today).
 * Calling the network slow needs an echo that is late by a network's worth.
 */
export const SLOW_LAG_MIN_MS = 750;
/** An echo this late needs no second opinion. */
export const SLOW_LAG_ALONE_MS = 1_500;
/** Quiet for this long and the episode is over. */
export const LINK_QUALITY_CLEAR_MS = 120_000;
/** How often the owner should ask whether the quiet window has elapsed. */
export const LINK_QUALITY_POLL_MS = 10_000;
/** How often the native late-request counter is read while the link is up. */
export const LINK_STATS_POLL_MS = 5_000;

export type LinkQualityChange =
  | { kind: "degraded"; state: LinkQualityState; losses: number; lagEvents: number; lateRequests: number }
  | { kind: "cleared"; afterMs: number };

/**
 * The sentence the user reads. Deliberately about *their* connection: the
 * app-shaped wording ("the bridge exited", "reconnecting…") is what left six
 * drops looking like six unrelated app failures.
 */
export function describeLinkQuality(state: LinkQualityState, host: string): string {
  return state === "unstable"
    ? `Your connection to ${host} is unstable; Muxflow keeps losing the link and reconnecting.`
    : `Your connection to ${host} is slow; the host is answering, but late.`;
}

export interface LinkQualityMonitor {
  /** The link dropped — one degraded episode, not one reconnect attempt. */
  noteLinkLost(at: number): LinkQualityChange | undefined;
  /**
   * One `input.echoLag` record with its lag; only lags of at least
   * `SLOW_LAG_MIN_MS` count.
   */
  noteEchoLag(at: number, lagMs: number): LinkQualityChange | undefined;
  /**
   * A host request the native side gave up waiting on (`requestLate`) and
   * kept the link through. Five seconds without an answer is the slow link
   * itself, not a symptom of it, so one is enough.
   */
  noteLateRequest(at: number): LinkQualityChange | undefined;
  /** Time passing is the only thing that ends an episode; call it on a bound. */
  poll(at: number): LinkQualityChange | undefined;
  /** A different host is a different connection; nothing carries over. */
  reset(): void;
  readonly state: LinkQualityState | undefined;
}

function prune(times: number[], at: number, windowMs: number): void {
  while (times.length > 0 && at - times[0] > windowMs) times.shift();
}

export function createLinkQualityMonitor(): LinkQualityMonitor {
  const losses: number[] = [];
  const lags: number[] = [];
  const lates: number[] = [];
  let state: LinkQualityState | undefined;
  /** Set by a signal that means "slow" on its own; consumed by `evaluate`. */
  let slowAlone = false;
  let episodeStartedAt = 0;
  let lastTriggerAt: number | undefined;

  const evaluate = (at: number): LinkQualityChange | undefined => {
    prune(losses, at, UNSTABLE_WINDOW_MS);
    prune(lags, at, SLOW_WINDOW_MS);
    prune(lates, at, SLOW_WINDOW_MS);
    const next = losses.length >= UNSTABLE_LOSS_COUNT
      ? "unstable"
      : lags.length >= SLOW_LAG_COUNT || slowAlone ? "slow" : undefined;
    slowAlone = false;
    if (next === undefined || next === state) return undefined;
    // A link that keeps dropping is also a link that echoes late, so an
    // episode may climb from slow to unstable — but never back down inside
    // the same episode, which would be two sentences for one bad network.
    if (state === "unstable") return undefined;
    if (state === undefined) episodeStartedAt = at;
    state = next;
    return {
      kind: "degraded", state: next, losses: losses.length, lagEvents: lags.length, lateRequests: lates.length,
    };
  };

  return {
    noteLinkLost(at) {
      losses.push(at);
      lastTriggerAt = at;
      return evaluate(at);
    },
    noteEchoLag(at, lagMs) {
      if (lagMs < SLOW_LAG_MIN_MS) return undefined;
      if (lagMs >= SLOW_LAG_ALONE_MS) slowAlone = true;
      lags.push(at);
      lastTriggerAt = at;
      return evaluate(at);
    },
    noteLateRequest(at) {
      slowAlone = true;
      lates.push(at);
      lastTriggerAt = at;
      return evaluate(at);
    },
    poll(at) {
      if (state === undefined || lastTriggerAt === undefined) return undefined;
      if (at - lastTriggerAt < LINK_QUALITY_CLEAR_MS) return undefined;
      const afterMs = at - episodeStartedAt;
      state = undefined;
      lastTriggerAt = undefined;
      losses.length = 0;
      lags.length = 0;
      lates.length = 0;
      return { kind: "cleared", afterMs };
    },
    reset() {
      state = undefined;
      lastTriggerAt = undefined;
      episodeStartedAt = 0;
      losses.length = 0;
      lags.length = 0;
      lates.length = 0;
    },
    get state() {
      return state;
    },
  };
}
