/**
 * Time bounds for the promises the pane visibility protocol has to wait on.
 *
 * Every wait in the reveal/hide handoff is on a promise that a wedged xterm
 * write completion can leave pending forever, and each of those waits used to
 * be unbounded: one stuck completion permanently removed a pane from the
 * protocol, which is one of the ways a pane freezes with no way back short of a
 * remount. A bound turns "never" into "late", and every caller here has a
 * defined answer for "late".
 */

export type TimedOutcome = "settled" | "timeout";

/**
 * Resolves with `work`'s value, or with `onTimeout()` when `work` has not
 * settled within `milliseconds`. A rejection from `work` still rejects, so a
 * caller that wants to treat failure as completion has to say so.
 *
 * The timer is cleared as soon as `work` settles, so a bounded wait costs
 * nothing once the common case wins the race.
 */
export function settleWithin<T>(work: Promise<T>, milliseconds: number, onTimeout: () => T): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let finished = false;
    const timer = setTimeout(() => {
      if (finished) return;
      finished = true;
      try {
        resolve(onTimeout());
      } catch (error) {
        reject(error);
      }
    }, milliseconds);
    work.then(
      (value) => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

/**
 * Waits for `work` to settle either way, giving up after `milliseconds`, and
 * reports which of the two happened. A rejection is a settlement: the caller
 * only wants to know whether the thing it was serializing behind is still
 * outstanding.
 */
export function awaitWithin(work: Promise<unknown>, milliseconds: number): Promise<TimedOutcome> {
  return settleWithin<TimedOutcome>(
    work.then(() => "settled" as const, () => "settled" as const),
    milliseconds,
    () => "timeout",
  );
}
