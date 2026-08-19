import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

export const RESUME_GAP_MS = 15_000;
const CHECK_INTERVAL_MS = 5_000;

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
export function useDesktopResumeRecovery(onResume: () => void): void {
  const callback = useRef(onResume);
  callback.current = onResume;
  useEffect(() => {
    if (typeof window === "undefined" || typeof document === "undefined") return;
    const monotonicNow = () => performance.now();
    const detector = new ResumeGapDetector(monotonicNow());
    const transitions = new ResumeTransitionDetector(typeof navigator !== "undefined" && !navigator.onLine);
    let lastRecoveryAt = Number.NEGATIVE_INFINITY;
    const recover = () => {
      const now = monotonicNow();
      if (now - lastRecoveryAt < 1_000) return;
      lastRecoveryAt = now;
      callback.current();
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
      if (detector.observe(monotonicNow())) recover();
    };
    const foregroundChanged = () => detector.reset(monotonicNow());
    const offline = () => { transitions.network(false); };
    const online = () => {
      if (transitions.network(true)) recover();
    };
    const timer = window.setInterval(check, CHECK_INTERVAL_MS);
    const nativeResume = listen("desktop-resumed", recover).catch(() => () => undefined);
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
