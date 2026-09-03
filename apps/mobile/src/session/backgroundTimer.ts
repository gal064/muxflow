// The native clock behind `protocol/backgroundTimer.ts`.
//
// React Native Android freezes every JavaScript timer while the host activity
// is paused (`JavaTimerManager.onHostPause` drops the timers frame callback
// unless a headless task is running), so a `setTimeout` armed from the
// background fires only when the app is next opened. The native module's
// `scheduleWake` runs the delay on an Android `Handler` instead, which the
// foreground service keeps alive, and answers with an `onWake` event that
// native→JS delivery still carries in the background.
//
// The delays that matter here are the ones nothing else would wake up for: the
// §7.2 reconnect backoff and the 60 s stable timer that resets its exponent,
// the handshake deadline and the notifier's post-settle wait. Per-request
// timeouts stay on `setTimeout`.

import { jsBackgroundTimer, type BackgroundTimer } from "../protocol/backgroundTimer";
import { muxflowSsh, type MuxflowSsh } from "../ssh/MuxflowSsh";
import { log } from "./log";

export { jsBackgroundTimer, type BackgroundTimer, type BackgroundTimerHandle } from "../protocol/backgroundTimer";

export type WakeClock = Pick<MuxflowSsh, "scheduleWake" | "cancelWake" | "addWakeListener">;

/**
 * Tokens are monotonic per timer, so a wake for a token that was cleared and
 * never reissued finds nothing to run. Should the native side refuse a
 * schedule (a lost react context), the delay falls back to `setTimeout` so the
 * timer still fires once the app is in the foreground rather than never.
 */
export function createNativeBackgroundTimer(ssh: WakeClock, report: (line: string) => void = log): BackgroundTimer {
  const pending = new Map<string, () => void>();
  let next = 0;
  let listening = false;

  const fire = (token: string): void => {
    const fn = pending.get(token);
    if (fn === undefined) return;
    pending.delete(token);
    fn();
  };

  return {
    set(delayMs, fn) {
      const token = String(++next);
      pending.set(token, fn);
      if (!listening) {
        listening = true;
        ssh.addWakeListener(fire);
      }
      ssh.scheduleWake(token, delayMs).catch((error: unknown) => {
        if (!pending.has(token)) return;
        report(`backgroundTimer: native schedule failed, using setTimeout: ${error instanceof Error ? error.message : String(error)}`);
        setTimeout(() => fire(token), delayMs);
      });
      return { token };
    },
    clear(handle) {
      if (!pending.delete(handle.token)) return;
      ssh.cancelWake(handle.token).catch(() => undefined);
    },
  };
}

let instance: BackgroundTimer | null = null;

/** The app-wide timer: native when the module is present, `setTimeout` otherwise (node, unit tests). */
export function backgroundTimer(): BackgroundTimer {
  if (instance === null) {
    let ssh: MuxflowSsh | null = null;
    try {
      ssh = muxflowSsh();
    } catch {
      log("backgroundTimer: no native module, using setTimeout");
    }
    instance = ssh === null ? jsBackgroundTimer : createNativeBackgroundTimer(ssh);
  }
  return instance;
}

/** `sleep` on the background timer, for callers that await a delay. */
export function backgroundSleep(ms: number): Promise<void> {
  return new Promise<void>((resolve) => backgroundTimer().set(ms, resolve));
}
