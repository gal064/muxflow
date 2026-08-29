/**
 * What a failed pane reveal means, and whether it is worth trying again.
 *
 * A reveal is how a mounted pane tells the host it is visible, and the host
 * starts every connection with all panes hidden — so a reveal that never lands
 * leaves the pane receiving no output at all. Two windows during a connection
 * (re)start reject it deterministically, and they need opposite answers.
 *
 * The bridge publishes its request writer — and opens its `ready` gate — one
 * round trip after it announces the epoch, so the reveal that epoch triggers
 * can arrive before there is anything to write to. That is not a conflict and
 * it clears on its own: resend.
 *
 * The other one does not clear by resending. `start_terminal` answers with the
 * new client id before the SSH handshake, and the bridge stamps its new epoch
 * into the client before it announces it, so a reveal issued in that window
 * carries a checkpoint epoch the host has already moved past — and no attempt
 * can re-stamp it, because the frame that would is the one still in flight.
 * That refusal replaces the request instead of repeating it; see
 * `STALE_REVEAL_EPOCH_CODE`.
 */

/**
 * The native rejections that mean "the transport is not ready yet".
 *
 * Matched as substrings of the stringified error because the host returns
 * these as plain strings. Each is a literal copy of `connection.rs` — two of
 * its sentences and one of its structured codes — so grep for it there before
 * editing it here, since a silently renamed message turns a fast retry back
 * into a two-second freeze.
 */
export const TRANSIENT_REVEAL_ERRORS = [
  "host bridge is disconnected",
  "terminal client is no longer attached",
  // The `ready` gate every request checks (`request_with_timeout`). The bridge
  // announces the epoch, then spends a round trip attaching before it sets
  // `ready`, so the reveal that epoch triggers lands in this refusal on every
  // reconnect (2026-08-28, the Wi-Fi-off episode: refused 6 ms after
  // `link.epoch`). Treating it as a conflict sent a reseed through the same
  // gate and surfaced the pair as an error toast; it clears on its own.
  "connection_unavailable",
] as const;

/**
 * The native side gave up waiting for the answer. Unlike the transient list
 * this is a *slow* failure — a whole response deadline each — so it is not
 * resent on the 250 ms ladder (eight of those would be minutes of a dark pane
 * and eight more unanswered requests on a link already behind); the watchdog's
 * own backoff re-asserts it. Quiet, though: it is the link, not the pane.
 */
const LATE_REVEAL_ERROR = "host request timed out";

function isLateRevealError(error: unknown): boolean {
  return String(error).includes(LATE_REVEAL_ERROR);
}

/** Failures that are the link's state, not news for the user. */
export function isQuietRevealError(error: unknown): boolean {
  return isTransientRevealError(error) || isLateRevealError(error);
}

/**
 * The one refusal a retry can never satisfy.
 *
 * A reveal carries a visibility checkpoint, and the checkpoint's epoch is the
 * one the frontend hub last saw on a `generationEpoch` frame. The host stamps
 * its own epoch into `client.terminal_epoch` *before* it publishes that frame
 * (connection/bridge.rs), so between a reconnect and the frame landing here
 * this side can only build checkpoints the host has already moved past —
 * exactly the sleep/wake window. Resending such a request is deterministic
 * failure: nothing between attempts can re-stamp the checkpoint, because only
 * the frame this side is still waiting for carries the new epoch. It was in the
 * transient list until it burned eight attempts over two seconds after every
 * wake, and the pane recovered from an unrelated fallback ~16s later.
 *
 * Matched on the structured code `connection.rs` prefixes the message with
 * (`STALE_VISIBILITY_EPOCH_CODE`), not on the human sentence behind it.
 */
export const STALE_REVEAL_EPOCH_CODE = "terminal_visibility_epoch_rejected";

export function isStaleEpochRevealError(error: unknown): boolean {
  return String(error).includes(STALE_REVEAL_EPOCH_CODE);
}

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
 * `retry` for a transport that is still coming up, `rebuild` when the request
 * itself is unsendable and has to be replaced by the checkpoint-free seed path,
 * `degrade` for everything else — a real conflict the watchdog and a host seed
 * have to resolve.
 */
export type RevealFailureAction = "ignore" | "retry" | "rebuild" | "degrade";

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
  // Before the retry budget is even consulted: this attempt's checkpoint is the
  // thing the host refused, so every attempt built from it fails identically.
  if (isStaleEpochRevealError(input.error)) return "rebuild";
  if (!isTransientRevealError(input.error)) return "degrade";
  return input.retriesUsed < REVEAL_RETRY_LIMIT ? "retry" : "degrade";
}
