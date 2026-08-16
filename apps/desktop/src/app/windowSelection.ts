import type { Window as TmuxWindow } from "./types";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";

export type WindowMoveDirection = "left" | "right";

/**
 * Admits only the newest remote navigation completion. Consecutive requests
 * for the exact same destination share their host request; an intervening
 * destination intentionally breaks coalescing because reusing older work could
 * let the host finish on the intervening target.
 */
export class RemoteNavigationCoordinator {
  #revision = 0;
  #pending?: { key: string; promise: Promise<TmuxActionResult | undefined> };

  async navigate(
    key: string,
    request: () => Promise<TmuxActionResult | undefined>,
    commit: () => void,
  ): Promise<boolean> {
    const revision = ++this.#revision;
    const pending = this.#pending?.key === key
      ? this.#pending
      : { key, promise: request() };
    this.#pending = pending;
    let accepted: TmuxActionResult | undefined;
    try {
      accepted = await pending.promise;
    } finally {
      if (this.#pending === pending) this.#pending = undefined;
    }
    if (!accepted || revision !== this.#revision) return false;
    commit();
    return true;
  }

  invalidate(): void {
    this.#revision += 1;
    this.#pending = undefined;
  }
}

export function relativeWindowReorderAction(
  windows: readonly TmuxWindow[],
  activeWindowId: string,
  direction: WindowMoveDirection,
): TmuxAction | undefined {
  const ordered = [...windows].sort((left, right) => left.index - right.index);
  const current = ordered.findIndex((window) => window.id === activeWindowId);
  const target = ordered[current + (direction === "left" ? -1 : 1)];
  const active = ordered[current];
  if (!active || !target || target.sessionId !== active.sessionId || target.id === active.id) return undefined;
  return {
    kind: "reorderWindow",
    sessionId: active.sessionId,
    windowId: active.id,
    targetWindowId: target.id,
    relativePosition: direction === "left" ? "before" : "after",
  };
}

export function resolveActiveWindowId(
  windows: readonly TmuxWindow[],
  current: string | undefined,
): string | undefined {
  return windows.find((window) => window.active)?.id
    ?? windows.find((window) => window.id === current)?.id
    ?? windows[0]?.id;
}

export async function requestActiveWindow(
  windows: readonly TmuxWindow[],
  current: string | undefined,
  requested: string,
  performAction: (action: TmuxAction) => Promise<TmuxActionResult | undefined>,
  setActiveWindowId: (windowId: string) => void,
): Promise<boolean> {
  const target = windows.find((window) => window.id === requested);
  if (!target || target.id === current) return false;
  const accepted = await performAction({
    kind: "selectWindow",
    sessionId: target.sessionId,
    windowId: target.id,
  });
  if (accepted) setActiveWindowId(target.id);
  return Boolean(accepted);
}
