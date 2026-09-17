/**
 * The one line a surface shows while something is genuinely still in flight.
 *
 * It is in the DOM from the first frame but invisible for 150ms, so a fast open
 * — the common case, and the one the reader notices — shows no loading text at
 * all: the frame, then the content. Only a load slow enough to be worth
 * explaining ever fades this in.
 *
 * The delay is CSS, not a timer, for two reasons: nothing here re-renders when
 * it elapses, and a static render — a test, a screenshot — still contains the
 * line, so assertions do not have to run the clock to see it.
 *
 * Every stage of one open uses the same wording and the same position, so two
 * stages that are both slow read as one indicator rather than as two loading
 * states handing off to each other.
 */
export function DelayedLoading({ detail }: { detail: string }) {
  return <p className="quiet-empty loading-delayed" role="status">{detail}</p>;
}
