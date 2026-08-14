import { useEffect, useRef, useState } from "react";
import { resizeClient } from "../features/terminal/api";
import { clientSizeForSurface } from "../features/terminal/clientSize";
import type { PixelBox, TerminalMeasurements } from "../features/terminal/TerminalRenderer";

/** Coalesces a drag of the window edge into one request, as before. */
export const CLIENT_RESIZE_DEBOUNCE_MS = 60;
/** Spacing and count of the retries after a request the bridge did not take. */
export const CLIENT_RESIZE_RETRY_MS = 500;
export const CLIENT_RESIZE_RETRIES = 4;

interface ClientResizeOptions {
  /** Recomputes when the workspace shows a different tmux window. */
  activeWindowId?: string;
  canMutate: boolean;
  clientId?: string;
  /** What a live terminal turns pixels into. Absent until one has mounted. */
  measurements?: TerminalMeasurements;
  onStatus(message: string): void;
}

/**
 * Owns the one request that can resize the user's real tmux windows.
 *
 * It is deliberately the only caller of `resizeClient`. tmux applies
 * `refresh-client -C` to the client that participates in sizing, and the
 * windows that client sees belong to a session other terminals may also be
 * attached to, so an over-large request damages sessions the app is not even
 * showing (P12-U006). The size therefore comes from the tiled surface's own
 * pixel box, never from a pane, and a size above the sane bound is reported
 * rather than sent.
 */
export function useClientResize({
  activeWindowId,
  canMutate,
  clientId,
  measurements,
  onStatus,
}: ClientResizeOptions): { surfaceRef: (element: HTMLElement | null) => void } {
  const [surface, setSurface] = useState<HTMLElement | null>(null);
  const [box, setBox] = useState<PixelBox>();
  const statusRef = useRef(onStatus);
  const lastRequested = useRef<{ clientId: string; columns: number; rows: number } | undefined>(undefined);
  const lastReported = useRef<string | undefined>(undefined);
  statusRef.current = onStatus;

  useEffect(() => {
    if (!surface) {
      // The surface unmounts whenever an app tab is showing. Forgetting its box
      // keeps a stale measurement from being sent as if it were current.
      setBox(undefined);
      return;
    }
    const observe = () => {
      const rect = surface.getBoundingClientRect();
      setBox((current) => current && current.width === rect.width && current.height === rect.height
        ? current
        : { width: rect.width, height: rect.height });
    };
    const observer = new ResizeObserver(observe);
    observer.observe(surface);
    observe();
    return () => observer.disconnect();
  }, [surface]);

  useEffect(() => {
    if (!clientId || !canMutate || !surface || !box) return;
    let disposed = false;
    let attempt = 0;
    let timer = 0;
    const report = (message: string) => {
      // One message per distinct problem. A window dragged past the bound would
      // otherwise repeat itself once per debounce.
      if (disposed || lastReported.current === message) return;
      lastReported.current = message;
      statusRef.current(message);
    };
    const send = () => {
      // Measured here rather than taken from the observer's last report. The
      // surface moves when the connection banner and the sidebars do, and the
      // request is gated on `canMutate`, so the box that arrives with the gate
      // reopening can already be one layout out of date — and a request built
      // from it moves tmux, which moves the banner, which moves the surface.
      // Reading the live box at send time is what stops that loop.
      const rect = surface.getBoundingClientRect();
      const decision = clientSizeForSurface({ width: rect.width, height: rect.height }, measurements);
      // `unavailable` is ordinary and silent: no terminal has reported metrics
      // yet (this effect re-runs when one does), the surface is hidden, or the
      // window is smaller than a cell.
      if (decision.kind === "unavailable") return;
      if (decision.kind === "refused") return report(decision.reason);
      const { columns, rows } = decision.size;
      const previous = lastRequested.current;
      // Every trigger recomputes; only a *different* answer reaches tmux.
      // Re-sending a size tmux already has is not free: the omarchy lane
      // measured 3 identical `refresh-client -C` requests costing 15
      // topology-dirty events, which is the churn this stage exists to remove.
      if (previous && previous.clientId === clientId && previous.columns === columns && previous.rows === rows) return;
      lastRequested.current = { clientId, columns, rows };
      lastReported.current = undefined;
      void resizeClient(clientId, columns, rows).catch((error) => {
        // The request never landed, so the next identical computation must not
        // be deduplicated away — unless a later request has already replaced
        // this record, in which case it is not ours to clear.
        const recorded = lastRequested.current;
        if (recorded?.clientId === clientId && recorded.columns === columns && recorded.rows === rows) {
          lastRequested.current = undefined;
        }
        // And retry, because nothing else will: the triggers are a window
        // change, a surface change and a reconnect. A bridge that rejects the
        // first resize after connect — the likeliest moment for one, 60 ms
        // after the gate opens — would otherwise leave the client at whatever
        // size the other terminals on that session set, for the whole session,
        // on a desktop nobody resizes.
        if (disposed) return;
        if (attempt < CLIENT_RESIZE_RETRIES) {
          attempt += 1;
          timer = window.setTimeout(send, CLIENT_RESIZE_RETRY_MS);
          return;
        }
        report(String(error));
      });
    };
    timer = window.setTimeout(send, CLIENT_RESIZE_DEBOUNCE_MS);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [activeWindowId, box, canMutate, clientId, measurements, surface]);

  return { surfaceRef: setSurface };
}
