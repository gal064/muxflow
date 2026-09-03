// A one-shot timer `HostConnection` runs its background delays on.
//
// React Native Android freezes every JavaScript timer while the host activity
// is paused, so the delays that have to elapse with the app in the background
// — the §7.2 backoff, the handshake deadline and the stable timer that resets
// its exponent — go through this interface.
// The app supplies a native clock (`src/session/backgroundTimer.ts`); the
// default, and what every unit test uses, is `setTimeout`.

export interface BackgroundTimerHandle {
  readonly token: string;
}

export interface BackgroundTimer {
  set(delayMs: number, fn: () => void): BackgroundTimerHandle;
  /** A cleared timer never runs its callback, even if a native wake for it arrives late. */
  clear(handle: BackgroundTimerHandle): void;
}

/** `setTimeout`, for tests and any runtime without the native module. */
export const jsBackgroundTimer: BackgroundTimer = createJsBackgroundTimer();

function createJsBackgroundTimer(): BackgroundTimer {
  const pending = new Map<string, ReturnType<typeof setTimeout>>();
  let next = 0;
  return {
    set(delayMs, fn) {
      const token = String(++next);
      pending.set(token, setTimeout(() => {
        pending.delete(token);
        fn();
      }, delayMs));
      return { token };
    },
    clear(handle) {
      const timer = pending.get(handle.token);
      if (timer === undefined) return;
      pending.delete(handle.token);
      clearTimeout(timer);
    },
  };
}
