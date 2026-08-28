import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

export const RESUME_GAP_MS = 15_000;
const CHECK_INTERVAL_MS = 5_000;
/**
 * How long a resume gives the existing link to answer before rebuilding it.
 *
 * A live link answers a correlated request in one RTT — tens of milliseconds
 * locally, a few hundred over a laptop's Wi-Fi. A link the suspend actually
 * killed never answers: the SSH keepalive notices that on its own schedule
 * (`ServerAliveInterval` × `ServerAliveCountMax`, about 45 s), which is far
 * too long to sit on a stale screen after opening the lid.
 */
export const RESUME_PROBE_TIMEOUT_MS = 3_000;

/** Which of the resume detectors asked for a recovery. */
export type ResumeTrigger = "native" | "timerGap" | "online";

export type ResumeProbeOutcome = "alive" | "dead";

/**
 * Decides whether a resume needs a rebuild by asking the link it already has.
 *
 * Every wake the native notification reports is not a link the suspend
 * killed: a dark wake (Power Nap) reconnects the native link on its own before
 * the lid opens, and a short sleep may never drop it at all. Rebuilding on
 * the notification alone replaced a healthy connection with a full snapshot
 * and reseed on every wake — the "System resumed; reconnecting…" toast with
 * the workspace jumping underneath it. One correlated request settles it: an
 * answer means the link is live and nothing needs to happen; an error or a
 * timeout means it is not, and the rebuild proceeds as before.
 */
export function probeResumedLink(
  probe: () => Promise<unknown>,
  timeoutMs = RESUME_PROBE_TIMEOUT_MS,
  setTimer: (callback: () => void, ms: number) => unknown = (callback, ms) => setTimeout(callback, ms),
): Promise<ResumeProbeOutcome> {
  return new Promise((resolve) => {
    let settled = false;
    const settle = (outcome: ResumeProbeOutcome) => {
      if (settled) return;
      settled = true;
      resolve(outcome);
    };
    setTimer(() => settle("dead"), timeoutMs);
    let request: Promise<unknown>;
    try {
      request = probe();
    } catch {
      settle("dead");
      return;
    }
    request.then(() => settle("alive"), () => settle("dead"));
  });
}

/** Detects a monotonic timer gap while avoiding repeated recoveries for one wake. */
export class ResumeGapDetector {
  #lastObservedAt: number;
  #lastRecoveryAt = Number.NEGATIVE_INFINITY;

  constructor(startedAt: number, readonly gapMs = RESUME_GAP_MS) {
    this.#lastObservedAt = startedAt;
  }

  observe(now: number): boolean {
    if (!Number.isFinite(now)) return false;
    const elapsed = now - this.#lastObservedAt;
    this.#lastObservedAt = now;
    if (elapsed < this.gapMs || now - this.#lastRecoveryAt < this.gapMs) return false;
    this.#lastRecoveryAt = now;
    return true;
  }

  /** Background throttling is not evidence of a machine suspend. */
  reset(now: number): void {
    if (Number.isFinite(now)) this.#lastObservedAt = now;
  }
}

export class ResumeTransitionDetector {
  #offline: boolean;

  constructor(offline: boolean) {
    this.#offline = offline;
  }

  network(online: boolean): boolean {
    const resumed = this.#offline && online;
    this.#offline = !online;
    return resumed;
  }
}

/**
 * Desktop suspend pauses WebKit timers. The first timer/focus/pageshow after
 * wake observes the gap and asks the authoritative bridge for a full reconnect
 * and snapshot. Ordinary focus changes do not reconnect.
 */
export function useDesktopResumeRecovery(onResume: (trigger: ResumeTrigger) => void): void {
  const callback = useRef(onResume);
  callback.current = onResume;
  useEffect(() => {
    if (typeof window === "undefined" || typeof document === "undefined") return;
    const monotonicNow = () => performance.now();
    const detector = new ResumeGapDetector(monotonicNow());
    const transitions = new ResumeTransitionDetector(typeof navigator !== "undefined" && !navigator.onLine);
    let lastRecoveryAt = Number.NEGATIVE_INFINITY;
    const recover = (trigger: ResumeTrigger) => {
      const now = monotonicNow();
      if (now - lastRecoveryAt < 1_000) return;
      lastRecoveryAt = now;
      callback.current(trigger);
    };
    const check = () => {
      // WebKit may pause timers while the window is hidden, minimized, on
      // another Space, or simply backgrounded. That gap is ordinary desktop
      // use, not a system resume, and reconnecting here created the startup
      // readiness race every time the person returned to the app.
      if (document.visibilityState !== "visible" || !document.hasFocus()) {
        detector.reset(monotonicNow());
        return;
      }
      if (detector.observe(monotonicNow())) recover("timerGap");
    };
    const foregroundChanged = () => detector.reset(monotonicNow());
    const offline = () => { transitions.network(false); };
    const online = () => {
      if (transitions.network(true)) recover("online");
    };
    const timer = window.setInterval(check, CHECK_INTERVAL_MS);
    const nativeResume = listen("desktop-resumed", () => recover("native")).catch(() => () => undefined);
    window.addEventListener("offline", offline);
    window.addEventListener("online", online);
    window.addEventListener("focus", foregroundChanged);
    window.addEventListener("blur", foregroundChanged);
    window.addEventListener("pageshow", foregroundChanged);
    document.addEventListener("visibilitychange", foregroundChanged);
    return () => {
      window.clearInterval(timer);
      void nativeResume.then((dispose) => dispose());
      window.removeEventListener("offline", offline);
      window.removeEventListener("online", online);
      window.removeEventListener("focus", foregroundChanged);
      window.removeEventListener("blur", foregroundChanged);
      window.removeEventListener("pageshow", foregroundChanged);
      document.removeEventListener("visibilitychange", foregroundChanged);
    };
  }, []);
}
