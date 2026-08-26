/**
 * Which status messages a person needs to see, and for how long.
 *
 * The app has one status channel that ~25 call sites write to. The previous
 * build rendered it permanently, in a banner above the terminal; Phase 11
 * deleted that banner, and for a while deleted the only *visible* reader with
 * it — every rejected action, every "no exact pane match", and the refusal the
 * client-size computation is designed to shout about went to a screen-reader
 * live region and nowhere else.
 *
 * This is the middle ground: the channel stays as it is, and this decides which
 * of its messages surface as a dismissible notice. It is deliberately a
 * denylist of the routine chatter rather than an allowlist, because a message
 * nobody classified should be shown, not swallowed — the failure mode this
 * exists to prevent.
 */

/**
 * Progress chatter the connection already reports through the host row, the
 * disconnected strip and the workspace list. Matched on the whole message, not
 * a substring, so a longer message that merely starts the same way still shows.
 */
const ROUTINE = new Set([
  "Live",
  "Discovering local tmux…",
  "Waiting for authoritative tmux state…",
  "Topology changed; reconciling…",
  "Terminal output is catching up…",
]);

/** Prefixes for messages that are ordinary progress with a variable tail. */
const ROUTINE_PREFIXES = [
  "Connecting to ",
  "Connection ",
  // No producer today — Item 19 deleted the one `Opened …` call site, because
  // the tab appearing is the message. Kept anyway, and tested: this is a
  // classification of a message *class*, not a record of who emits it, and
  // "the user can already see it" stays true of any future `Opened X`.
  "Opened ",
  "Viewing the last known ",
  "Renaming ",
  "Launching ",
  "Resuming ",
];

/**
 * Internal bookkeeping that fails as a *consequence* of something already being
 * reported, and never as the thing a person should act on.
 *
 * Marking a pane hidden or visible is how the app tells the host which panes it
 * is showing. When the link drops, every mounted pane's teardown fails at once
 * and the last one wins the status channel — so a disconnect surfaced as a
 * permanent red alert reading "Could not mark %129 hidden: terminal client is
 * no longer attached", naming an internal pane id, offering nothing to do, and
 * never auto-dismissing. Measured on the packaged app. The disconnected strip
 * and the host row already say the true thing, and the full text stays in the
 * live region.
 *
 * Matched on the whole message shape, so a bookkeeping failure that is *not* a
 * disconnect consequence still shows.
 */
const CONSEQUENTIAL = [
  /^Could not mark %\d+ (hidden|visible): /u,
  /^Could not mark agent attention seen: /u,
];

export interface StatusNotice {
  message: string;
  /** Distinguishes two identical messages so a repeat re-shows the notice. */
  id: number;
  severity: "info" | "problem";
}

/** Words that mean the app refused, failed, or could not do something. */
const PROBLEM = /(\bcannot\b|\bcould not\b|\bunavailable\b|\bfailed\b|\bfailure\b|\berror\b|refus|rejected\b|\bno longer\b|\bnot available\b|\bfrozen\b|\bstopped\b|\btimed out\b|\bunknown\b|\bexceeds\b|\brequire[sd]?\b|\bno agent is waiting\b|\bleft open\b)/iu;

export function noticeForStatus(message: string, id: number): StatusNotice | undefined {
  const trimmed = message.trim();
  if (trimmed === "") return undefined;
  if (ROUTINE.has(trimmed)) return undefined;
  if (ROUTINE_PREFIXES.some((prefix) => trimmed.startsWith(prefix))) return undefined;
  if (CONSEQUENTIAL.some((pattern) => pattern.test(trimmed))) return undefined;
  return { message: trimmed, id, severity: PROBLEM.test(trimmed) ? "problem" : "info" };
}

/**
 * How long an informational notice stays up. Problems do not auto-dismiss:
 * something the app refused to do is the user's to acknowledge, and a notice
 * that vanishes before it is read is the defect this module exists to fix.
 */
export const NOTICE_DISMISS_MS = 6_000;

export function noticeDismissDelay(notice: StatusNotice): number | undefined {
  return notice.severity === "info" ? NOTICE_DISMISS_MS : undefined;
}
