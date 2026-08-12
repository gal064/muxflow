import type { PersistedAppState } from "./types";

export type WindowGeometry = NonNullable<PersistedAppState["shell"]["windowGeometry"]>;
export interface PhysicalMonitorGeometry {
  position: { x: number; y: number };
  size: { width: number; height: number };
  scaleFactor: number;
}

const MIN_SCALE = 0.5;
const MAX_SCALE = 8;

function safeScale(value: number): number {
  return Number.isFinite(value) && value >= MIN_SCALE && value <= MAX_SCALE ? value : 1;
}

/** Persist physical geometry with its scale so a 200% monitor keeps the same logical window size. */
export function captureWindowGeometry(
  position: { x: number; y: number },
  size: { width: number; height: number },
  maximized: boolean,
  scaleFactor: number,
): WindowGeometry {
  return {
    x: Math.round(position.x),
    y: Math.round(position.y),
    width: Math.round(size.width),
    height: Math.round(size.height),
    maximized,
    scaleFactorMilli: Math.round(safeScale(scaleFactor) * 1_000),
  };
}

export function restoredPhysicalSize(
  geometry: WindowGeometry,
  currentScaleFactor: number,
): { width: number; height: number } {
  // State written before scale-aware geometry shipped is intentionally
  // interpreted as physical pixels to preserve its historical behavior.
  if (geometry.scaleFactorMilli === undefined) {
    return { width: geometry.width, height: geometry.height };
  }
  const capturedScale = safeScale(geometry.scaleFactorMilli / 1_000);
  const currentScale = safeScale(currentScaleFactor);
  return {
    width: Math.max(320, Math.round(geometry.width * currentScale / capturedScale)),
    height: Math.max(240, Math.round(geometry.height * currentScale / capturedScale)),
  };
}

export function restoredWindowGeometry(
  geometry: WindowGeometry,
  monitors: readonly PhysicalMonitorGeometry[],
  primary: PhysicalMonitorGeometry | null,
  fallbackScaleFactor: number,
): { x: number; y: number; width: number; height: number } {
  const intersects = (monitor: PhysicalMonitorGeometry): boolean => {
    const right = Math.min(geometry.x + geometry.width, monitor.position.x + monitor.size.width);
    const bottom = Math.min(geometry.y + geometry.height, monitor.position.y + monitor.size.height);
    return right > Math.max(geometry.x, monitor.position.x)
      && bottom > Math.max(geometry.y, monitor.position.y);
  };
  const monitor = monitors.find(intersects) ?? primary ?? monitors[0] ?? null;
  const size = restoredPhysicalSize(geometry, monitor?.scaleFactor ?? fallbackScaleFactor);
  if (!monitor) return { x: geometry.x, y: geometry.y, ...size };
  if (!monitors.some(intersects)) {
    return {
      x: Math.round(monitor.position.x + (monitor.size.width - size.width) / 2),
      y: Math.round(monitor.position.y + (monitor.size.height - size.height) / 2),
      ...size,
    };
  }
  const minimumVisible = 64;
  return {
    x: Math.min(
      monitor.position.x + monitor.size.width - minimumVisible,
      Math.max(monitor.position.x - size.width + minimumVisible, geometry.x),
    ),
    y: Math.min(
      monitor.position.y + monitor.size.height - minimumVisible,
      Math.max(monitor.position.y - size.height + minimumVisible, geometry.y),
    ),
    ...size,
  };
}
