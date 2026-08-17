/**
 * Answering an abandoned caller now, and stopping the host as well.
 *
 * Three surfaces needed the same thing and, before this module, each built it:
 * a rejectable promise raced against the real answer, an `abort` listener that
 * runs one side effect and rejects, and a `finally` that removes the listener.
 * The Git lane had it as a helper; the Explorer lane had two hand-written
 * copies of the same twenty lines. One implementation is the point — the
 * semantics here are subtle enough that three of them will diverge:
 *
 * - The caller is answered by the *race*, not by the inner promise. Telling the
 *   host to stop and then continuing to await it leaves the abort invisible:
 *   the promise still resolves with whatever the host had already produced, and
 *   a listing gets installed into a directory the tree has since collapsed.
 * - The inner promise is not cancelled and is allowed to settle unobserved. It
 *   belongs to whoever created it, and on a shared request it may be several
 *   other callers' answer.
 * - `onAbort` runs exactly once, whether the signal was already aborted on
 *   entry or fires later, and its own failure never becomes the caller's.
 *
 * What each caller supplies is only what differs: the side effect that stops
 * the remote work or releases the shared claim, and the sentence the rejection
 * carries.
 */

/** The rejection an abandoned caller sees. Always an `AbortError`. */
export function cancelled(reason: string): DOMException {
  return new DOMException(reason, "AbortError");
}

/**
 * Refuses before any work is started.
 *
 * Distinct from the already-aborted path inside [`abortable`]: nothing has been
 * asked of the host yet, so there is nothing to stop and no side effect to run.
 */
export function throwIfAborted(signal: AbortSignal | undefined, reason: string): void {
  if (signal?.aborted) throw cancelled(reason);
}

/**
 * Settles with `promise`, or rejects as soon as `signal` aborts.
 *
 * `onAbort` is the caller's own stop: a remote cancellation, a refcount
 * release, or both. It is invoked once and its result is discarded, because an
 * abort that cannot be delivered to the host is still an abort as far as this
 * caller is concerned.
 */
export function abortable<T>(
  promise: Promise<T>,
  signal: AbortSignal | undefined,
  onAbort: () => void,
  reason: string,
): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) {
    onAbort();
    return Promise.reject(cancelled(reason));
  }
  return new Promise<T>((resolve, reject) => {
    const abandon = () => {
      onAbort();
      reject(cancelled(reason));
    };
    signal.addEventListener("abort", abandon, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", abandon));
  });
}
