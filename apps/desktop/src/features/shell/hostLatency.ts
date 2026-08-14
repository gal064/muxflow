import { useEffect, useState } from "react";

/**
 * The number in the sidebar's host row.
 *
 * It is measured, never polled. Phase 12 removed every periodic round-trip on
 * purpose, so a latency readout that pinged the host would put one straight
 * back; instead this records how long the round-trips the app *already* makes
 * take, and the host row shows the most recent one.
 *
 * That means the number can go stale on a session where nothing is happening.
 * It is therefore stamped, `STALE_AFTER_MS` old readings are not shown at all,
 * and the row's tooltip says what the number is. A blank latency is honest; a
 * fabricated one is not.
 */
export interface HostLatencySample {
  milliseconds: number;
  at: number;
}

/** Beyond this, the last measurement says nothing about the link right now. */
export const STALE_AFTER_MS = 60_000;

/** Smoothing factor: enough to absorb one slow action, not enough to lie. */
const SMOOTHING = 0.3;

const listeners = new Set<(sample: HostLatencySample | undefined) => void>();
let current: HostLatencySample | undefined;

export function recordHostRoundTrip(milliseconds: number, now = Date.now()): void {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return;
  const previous = current && now - current.at <= STALE_AFTER_MS ? current.milliseconds : undefined;
  const smoothed = previous === undefined ? milliseconds : previous + (milliseconds - previous) * SMOOTHING;
  current = { milliseconds: smoothed, at: now };
  for (const listener of listeners) listener(current);
}

/** A new bridge measures a new link; whatever the last one saw is not it. */
export function resetHostLatency(): void {
  current = undefined;
  for (const listener of listeners) listener(undefined);
}

export function hostLatency(now = Date.now()): HostLatencySample | undefined {
  return current && now - current.at <= STALE_AFTER_MS ? current : undefined;
}

/** Times one promise and records it, without changing what the caller sees. */
export async function measureHostRoundTrip<T>(work: Promise<T>): Promise<T> {
  const started = performance.now();
  try {
    return await work;
  } finally {
    recordHostRoundTrip(performance.now() - started);
  }
}

export function useHostLatency(): HostLatencySample | undefined {
  const [sample, setSample] = useState(() => hostLatency());
  useEffect(() => {
    const listener = (next: HostLatencySample | undefined) => setSample(next);
    listeners.add(listener);
    // The reading expires on its own, so the row has to re-check even when
    // nothing new arrives.
    const timer = window.setInterval(() => setSample(hostLatency()), 5_000);
    return () => {
      listeners.delete(listener);
      window.clearInterval(timer);
    };
  }, []);
  return sample;
}
