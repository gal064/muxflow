import { useEffect, useRef, useState } from "react";
import { resizeClient } from "../features/terminal/api";
import { clientSizeForSurface } from "../features/terminal/clientSize";
import type { PixelBox, TerminalSize } from "../features/terminal/TerminalRenderer";

/** Coalesces a drag of the window edge into one request, as before. */
export const CLIENT_RESIZE_DEBOUNCE_MS = 60;
/** Spacing and count of the retries that wait for a terminal to report metrics. */
export const CLIENT_RESIZE_RETRY_MS = 250;
export const CLIENT_RESIZE_RETRIES = 8;

interface ClientResizeOptions {
  /** Recomputes when the workspace shows a different tmux window. */
  activeWindowId?: string;
  canMutate: boolean;
  clientId?: string;
  /**
   * Changes whenever a terminal mounts or unmounts. A computation that ran
   * before any renderer existed has no cell metrics; this is what brings it
   * back as soon as one is alive, without waiting for the retry timer.
   */
  metricsKey?: string | number;
  /** Cells that fit a pixel box, from a live terminal's font metrics. */
  measureBox(box: PixelBox): TerminalSize | undefined;
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
  measureBox,
  metricsKey,
  onStatus,
}: ClientResizeOptions): { surfaceRef: (element: HTMLElement | null) => void } {
  const [surface, setSurface] = useState<HTMLElement | null>(null);
  const [box, setBox] = useState<PixelBox>();
  const measureBoxRef = useRef(measureBox);
  const statusRef = useRef(onStatus);
  const lastRequested = useRef<{ clientId: string; columns: number; rows: number } | undefined>(undefined);
  const reportedUnavailable = useRef<string | undefined>(undefined);
  measureBoxRef.current = measureBox;
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
    const send = () => {
      // Measured here rather than taken from the observer's last report. The
      // surface moves while the connection banner grows and shrinks, and the
      // request is gated on `canMutate`, so the box that arrives with the gate
      // reopening can already be one layout out of date — and a request built
      // from it moves tmux, which moves the banner, which moves the surface.
      // Reading the live box at send time is what stops that loop.
      const rect = surface.getBoundingClientRect();
      const decision = clientSizeForSurface({ width: rect.width, height: rect.height }, (candidate) => measureBoxRef.current(candidate));
      if (decision.kind === "unavailable") {
        if (!decision.retry) return;
        if (attempt < CLIENT_RESIZE_RETRIES) {
          attempt += 1;
          timer = window.setTimeout(send, CLIENT_RESIZE_RETRY_MS);
          return;
        }
        // Out of retries: the app is connected, has a surface, and cannot size
        // its client. Silence here is how a broken measurement would look
        // exactly like a correct one (P12-U006's quiet direction).
        if (reportedUnavailable.current !== decision.reason) {
          reportedUnavailable.current = decision.reason;
          statusRef.current(`The tmux client size could not be computed: ${decision.reason}.`);
        }
        return;
      }
      reportedUnavailable.current = undefined;
      if (decision.kind === "refused") {
        statusRef.current(decision.reason);
        return;
      }
      const { columns, rows } = decision.size;
      const previous = lastRequested.current;
      // Every trigger recomputes; only a *different* answer reaches tmux. A
      // repeated identical `refresh-client -C` is not free on a shared session:
      // it re-asserts this client as the one tmux last sized for, which tugs at
      // the plain terminals attached to the same session for no gain.
      if (previous && previous.clientId === clientId && previous.columns === columns && previous.rows === rows) return;
      lastRequested.current = { clientId, columns, rows };
      void resizeClient(clientId, columns, rows).catch((error) => {
        // The request never landed, so the next identical computation must not
        // be deduplicated away — unless a later request has already replaced
        // this record, in which case it is not ours to clear.
        const recorded = lastRequested.current;
        if (recorded?.clientId === clientId && recorded.columns === columns && recorded.rows === rows) {
          lastRequested.current = undefined;
        }
        // A rejection from a bridge that has already been replaced is not this
        // connection's status.
        if (!disposed) statusRef.current(String(error));
      });
    };
    timer = window.setTimeout(send, CLIENT_RESIZE_DEBOUNCE_MS);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [activeWindowId, box, canMutate, clientId, metricsKey, surface]);

  return { surfaceRef: setSurface };
}
