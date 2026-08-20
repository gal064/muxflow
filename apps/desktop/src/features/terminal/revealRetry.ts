/**
 * What a failed pane reveal means, and whether it is worth trying again.
 *
 * A reveal is how a mounted pane tells the host it is visible, and the host
 * starts every connection with all panes hidden — so a reveal that never lands
 * leaves the pane receiving no output at all. Two windows during a connection
 * (re)start reject it deterministically: `start_terminal` answers with the new
 * client id before the SSH handshake, so a reveal issued then carries a
 * terminal epoch the host has already moved past, and the bridge publishes its
 * request writer one round trip after it announces the epoch, so the reveal
 * that epoch triggers arrives before there is anything to write to. Neither is
 * a conflict to recover from; both clear on their own within a round trip.
 */

/**
 * The native rejections that mean "the transport is not ready yet".
 *
 * Matched as substrings of the stringified error because the host returns
 * these as plain strings. They are literal copies of `connection.rs` — grep
 * for a phrase there before editing it here, since a silently renamed message
 * turns a fast retry back into a two-second freeze.
 */
export const TRANSIENT_REVEAL_ERRORS = [
  "stale connection epoch",
  "host bridge is disconnected",
  "terminal client is no longer attached",
] as const;

/**
 * Fast enough that the pane is revealed within a frame or two of the bridge
 * becoming ready, and bounded so the series ends where the pane watchdog's
 * first re-assertion begins (+2s) rather than duplicating it.
 */
export const REVEAL_RETRY_DELAY_MS = 250;
export const REVEAL_RETRY_LIMIT = 8;

export function isTransientRevealError(error: unknown): boolean {
  const text = String(error);
  return TRANSIENT_REVEAL_ERRORS.some((phrase) => text.includes(phrase));
}

/**
 * `ignore` when the failed attempt was superseded or its pane unmounted,
 * `retry` for a transport that is still coming up, `degrade` for everything
 * else — a real conflict the watchdog and a host seed have to resolve.
 */
export type RevealFailureAction = "ignore" | "retry" | "degrade";

export function revealFailureAction(input: {
  error: unknown;
  /** Whether the attempt that failed is still the pane's current reveal. */
  current: boolean;
  /** Retries already spent on this attempt. */
  retriesUsed: number;
}): RevealFailureAction {
  // A superseded failure says nothing about the pane's health: a newer reveal
  // owns the outcome, and marking the pane degraded from here can latch a
  // by-then-healthy pane into the watchdog's retry loop.
  if (!input.current) return "ignore";
  if (!isTransientRevealError(input.error)) return "degrade";
  return input.retriesUsed < REVEAL_RETRY_LIMIT ? "retry" : "degrade";
}
