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
}

export class ResumeTransitionDetector {
  #hidden: boolean;
  #offline: boolean;

  constructor(hidden: boolean, offline: boolean) {
    this.#hidden = hidden;
    this.#offline = offline;
  }

  visibility(hidden: boolean): boolean {
    const resumed = this.#hidden && !hidden;
    this.#hidden = hidden;
    return resumed;
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
    const transitions = new ResumeTransitionDetector(
      document.visibilityState === "hidden",
      typeof navigator !== "undefined" && !navigator.onLine,
    );
    let lastRecoveryAt = Number.NEGATIVE_INFINITY;
    const recover = () => {
      const now = monotonicNow();
      if (now - lastRecoveryAt < 1_000) return;
      lastRecoveryAt = now;
      callback.current();
    };
    const check = () => {
      if (detector.observe(monotonicNow())) recover();
    };
    const visibilityChanged = () => {
      if (transitions.visibility(document.visibilityState === "hidden")) recover();
    };
    const offline = () => { transitions.network(false); };
    const online = () => {
      if (transitions.network(true)) recover();
    };
    const timer = window.setInterval(check, CHECK_INTERVAL_MS);
    const nativeResume = listen("desktop-resumed", recover).catch(() => () => undefined);
    window.addEventListener("offline", offline);
    window.addEventListener("online", online);
    window.addEventListener("pageshow", check);
    document.addEventListener("visibilitychange", visibilityChanged);
    return () => {
      window.clearInterval(timer);
      void nativeResume.then((dispose) => dispose());
      window.removeEventListener("offline", offline);
      window.removeEventListener("online", online);
      window.removeEventListener("pageshow", check);
      document.removeEventListener("visibilitychange", visibilityChanged);
    };
  }, []);
}
