import { useCallback, useEffect, useRef, useState } from "react";
import { resizeClient } from "../features/terminal/api";
import { clientSizeForSurface } from "../features/terminal/clientSize";
import type { PixelBox, TerminalMeasurements, TerminalSize } from "../features/terminal/TerminalRenderer";

/** Coalesces a drag of the window edge into one request, as before. */
export const CLIENT_RESIZE_DEBOUNCE_MS = 60;
/** Spacing and count of the retries after a request the bridge did not take. */
export const CLIENT_RESIZE_RETRY_MS = 500;
export const CLIENT_RESIZE_RETRIES = 4;
/**
 * How many times the app may take the client size back from another terminal
 * between one focus gain and the next.
 *
 * A budget rather than a rule about *which* mismatch, because the failure this
 * bounds is a fight and not a repetition: two clients that each re-assert on
 * seeing the other's size resize the user's real windows back and forth for as
 * long as both are attached. Two attempts is enough to win against a terminal
 * that moved the windows once and went quiet, and few enough that losing is
 * quiet too — the user's next return to the app buys two more.
 */
export const CLIENT_RESIZE_REASSERTS = 2;

interface ClientResizeOptions {
  /** Recomputes when the workspace shows a different tmux window. */
  activeWindowId?: string;
  /**
   * The cell grid tmux actually has for that window, from the authoritative
   * snapshot — not what this hook asked for. Undefined while there is no
   * window to describe.
   */
  actualSize?: TerminalSize;
  /**
   * Whether the app's own OS window has focus, which decides whether it may
   * take the client size back from another terminal.
   */
  appFocused: boolean;
  canMutate: boolean;
  clientId?: string;
  onStatus(message: string): void;
}

interface ClientResize {
  /** Receives the tiled surface element the client size is measured from. */
  surfaceRef: (element: HTMLElement | null) => void;
  /** Receives what a live terminal turns pixels into. */
  onMeasurements: (measurements: TerminalMeasurements) => void;
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
 *
 * Everything the request depends on lives in refs, and the one in-flight
 * request and its retries live in `pending`. That is not incidental: a request
 * outlives the effect that started it — a window switch or a sidebar toggle
 * re-runs the effect while the bridge is still answering — and a retry tied to
 * an effect's lifetime is a retry that quietly disappears exactly when the app
 * is busiest.
 */
export function useClientResize({
  activeWindowId,
  actualSize,
  appFocused,
  canMutate,
  clientId,
  onStatus,
}: ClientResizeOptions): ClientResize {
  const [surface, setSurface] = useState<HTMLElement | null>(null);
  const [box, setBox] = useState<PixelBox>();
  const [measurements, setMeasurements] = useState<TerminalMeasurements>();
  const statusRef = useRef(onStatus);
  const surfaceRefValue = useRef(surface);
  const measurementsRef = useRef(measurements);
  const clientIdRef = useRef(clientId);
  const canMutateRef = useRef(canMutate);
  const lastRequested = useRef<{ clientId: string; columns: number; rows: number } | undefined>(undefined);
  const lastReported = useRef<string | undefined>(undefined);
  const pending = useRef<{ timer: number; attempt: number }>({ timer: 0, attempt: 0 });
  /** Reassertions spent since the app last gained focus. */
  const reasserted = useRef(0);
  statusRef.current = onStatus;
  surfaceRefValue.current = surface;
  measurementsRef.current = measurements;
  clientIdRef.current = clientId;
  canMutateRef.current = canMutate;

  const send = useCallback(() => {
    const element = surfaceRefValue.current;
    const currentClientId = clientIdRef.current;
    if (!element || !currentClientId || !canMutateRef.current) return;
    // Measured here rather than taken from the observer's last report. The
    // surface moves when the connection banner and the sidebars do, and the
    // request is gated on `canMutate`, so the box that arrives with the gate
    // reopening can already be one layout out of date — and a request built
    // from it moves tmux, which moves the banner, which moves the surface.
    // Reading the live box at send time is what stops that loop.
    const rect = element.getBoundingClientRect();
    const decision = clientSizeForSurface({ width: rect.width, height: rect.height }, measurementsRef.current);
    if (decision.kind === "none") return;
    if (decision.kind === "refused") {
      // Once per distinct problem: a window dragged past the bound would
      // otherwise repeat itself every debounce.
      if (lastReported.current === decision.reason) return;
      lastReported.current = decision.reason;
      statusRef.current(decision.reason);
      return;
    }
    const { columns, rows } = decision.size;
    const previous = lastRequested.current;
    // Every trigger recomputes; only a *different* answer reaches tmux.
    // Re-sending a size tmux already has is not free: the remote-linux lane measured
    // 3 identical `refresh-client -C` requests costing 15 topology-dirty events
    // on the real link (6 on a local one), which is the churn this stage exists
    // to remove.
    if (previous && previous.clientId === currentClientId && previous.columns === columns && previous.rows === rows) return;
    lastRequested.current = { clientId: currentClientId, columns, rows };
    lastReported.current = undefined;
    void resizeClient(currentClientId, columns, rows).then(() => {
      // The retry budget is per *failure run*, not per connection. Counting it
      // across the whole connection meant four transient failures early on left
      // every later failure — including the first resize after a reconnect, the
      // one the retry exists for — with no retry at all. Reset on a landed
      // request, never on an attempted one, or the retry loop never terminates.
      pending.current.attempt = 0;
    }).catch((error) => {
      // The request never landed, so the next identical computation must not be
      // deduplicated away — unless a later request already replaced this
      // record, in which case it is not ours to clear.
      const recorded = lastRequested.current;
      if (recorded?.clientId === currentClientId && recorded.columns === columns && recorded.rows === rows) {
        lastRequested.current = undefined;
      }
      // And retry, because nothing else will: the triggers are a window change,
      // a surface change and a reconnect. A bridge that rejects the first
      // resize after connect — the likeliest moment for one — would otherwise
      // leave the client at whatever size the other terminals on that session
      // set, for the whole session, on a desktop nobody resizes.
      if (clientIdRef.current !== currentClientId) return;
      if (pending.current.attempt < CLIENT_RESIZE_RETRIES) {
        pending.current.attempt += 1;
        window.clearTimeout(pending.current.timer);
        pending.current.timer = window.setTimeout(send, CLIENT_RESIZE_RETRY_MS);
        return;
      }
      const message = String(error);
      if (lastReported.current === message) return;
      lastReported.current = message;
      statusRef.current(message);
    });
  }, []);

  useEffect(() => {
    if (!surface) {
      // Nothing to measure yet, or nothing left to measure. An app tab no
      // longer takes the surface away — it is covered, not unmounted, so its
      // box stays valid and this stops churning to `undefined` and back on
      // every tab switch. Forgetting the box when the element really does go
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

  // A new bridge is a new tmux client: whatever the last one was told, and
  // whatever it still owed, belongs to a connection that is gone.
  useEffect(() => {
    lastRequested.current = undefined;
    lastReported.current = undefined;
    reasserted.current = 0;
    pending.current.attempt = 0;
    window.clearTimeout(pending.current.timer);
  }, [clientId]);

  useEffect(() => {
    if (!clientId || !canMutate || !surface || !box || !measurements) return;
    const timer = window.setTimeout(send, CLIENT_RESIZE_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [activeWindowId, box, canMutate, clientId, measurements, send, surface]);

  // Coming back to the app is a fresh opportunity, and the one moment under
  // `window-size latest` semantics when this app should be the sizing client.
  // Declared before the reconciliation so that on the render where focus
  // returns, the budget is restored before it is read.
  useEffect(() => {
    if (appFocused) reasserted.current = 0;
  }, [appFocused]);

  /**
   * Re-asserts the size when tmux's answer is not the one that was asked for.
   *
   * The dedupe above compares each computation against *what this hook last
   * requested*, which is only the same thing as what tmux has for as long as
   * nothing else moves the windows. Under `window-size latest` something else
   * routinely does: a plain terminal attached to the same session, whose own
   * activity makes it the latest client. The app then letterboxes and has
   * nothing to notice it with — its surface never moved, so it never recomputes
   * a different answer, and input through a control client does not make that
   * client the latest either. Comparing against the snapshot is what turns the
   * dedupe from "what I last asked" into "what tmux actually has".
   *
   * Only while the app has focus. The asymmetry is the whole anti-resize-war
   * policy: an unfocused app is one the user has deliberately left to work in
   * that other terminal, and resizing its windows out from under them is
   * P12-U006's class of harm. Focused, the user is here, and here is where the
   * size should come from.
   *
   * The ordinary path above is deliberately *not* gated on focus: a surface
   * that changed size is this app's own geometry changing, which is a new fact
   * to state rather than a size to fight over.
   *
   * This runs only when tmux's answer *changes*, because that is what the
   * dependencies are, so a size tmux simply never takes costs one attempt and
   * then nothing. The budget is there for the other shape: another client that
   * answers every reassertion with its own, which without a bound is two
   * programs resizing a real person's windows at each other indefinitely.
   */
  useEffect(() => {
    // `surface` because a request needs one: with an app tab showing there is
    // no tiled surface to measure, `send` returns immediately, and spending a
    // reassertion on a request that cannot be built would leave the budget
    // gone when the terminal comes back.
    if (!appFocused || !clientId || !canMutate || !actualSize || !surface) return;
    const requested = lastRequested.current;
    if (!requested || requested.clientId !== clientId) return;
    if (requested.columns === actualSize.columns && requested.rows === actualSize.rows) return;
    if (reasserted.current >= CLIENT_RESIZE_REASSERTS) return;
    const timer = window.setTimeout(() => {
      // Both of these happen here rather than above so that an effect re-run
      // cancelling this timer leaves nothing half-applied: the budget is spent
      // only on an attempt actually made, and the record `lastRequested` holds
      // is the one the next run needs to recognise the mismatch at all.
      // Forgetting it is what lets the identical computation through the
      // dedupe; `send` recomputes from the live surface either way.
      reasserted.current += 1;
      lastRequested.current = undefined;
      send();
    }, CLIENT_RESIZE_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [actualSize?.columns, actualSize?.rows, appFocused, canMutate, clientId, send, surface]);

  // The retry timer is the one thing that outlives an effect; only unmounting
  // ends it.
  useEffect(() => () => window.clearTimeout(pending.current.timer), []);

  return {
    surfaceRef: setSurface,
    onMeasurements: useCallback((next: TerminalMeasurements) => {
      // Two terminals report the same numbers; a new object every time would
      // restart the computation for nothing.
      setMeasurements((current) => sameMeasurements(current, next) ? current : next);
    }, []),
  };
}

function sameMeasurements(left: TerminalMeasurements | undefined, right: TerminalMeasurements): boolean {
  return left !== undefined
    && left.cell.width === right.cell.width && left.cell.height === right.cell.height
    && left.chrome.horizontal === right.chrome.horizontal
    && left.chrome.vertical === right.chrome.vertical
    && left.chrome.scrollbar === right.chrome.scrollbar;
}
