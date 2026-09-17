// The JSON contract between the app and the terminal WebView page (§10.2).
// Imported by both sides so the two cannot drift.

/** RN → page. */
export type ToPageMessage =
  | { t: "init" }
  | { t: "seed"; b64: string }
  | { t: "out"; b64: string }
  /**
   * §7.6.1: rebuild the buffer as scrollback + screen. `hist` is every page
   * fetched so far (rows joined with CRLF, topmost first), `rowsAdded` how
   * many of them the newest page contributed, `tail` everything the controller
   * has fed xterm since the seed — the page resets, writes the history, scrolls
   * it fully above the display, replays the tail, and puts the viewport back
   * `rowsAdded` rows below the top: the row the reader was looking at.
   */
  | { t: "splice"; hist: string; rowsAdded: number; tail: string }
  | { t: "measure" };

/** page → RN. */
export type FromPageMessage =
  | { t: "ready" }
  | { t: "size"; cols: number; rows: number; cellWidth?: number; cellHeight?: number }
  | { t: "written"; bytes: number }
  /** Alternate-screen touch scrolling, encoded by xterm as mouse or cursor input. */
  | { t: "input"; b64: string }
  /**
   * The reader hit the top of the buffer on the normal screen: `above` is how
   * many scrollback rows the page already holds, which becomes the request's
   * `skip`. Throttled by the page; the controller decides whether to ask.
   */
  | { t: "atTop"; above: number }
  /** One content-free summary after a touch gesture; never emitted per move/frame. */
  | { t: "scroll"; mode: "normal" | "alternate"; rows: number; durationMs: number; cancelled: boolean }
  /** Explicit user actions only; terminal content is never sent for observation or logging. */
  | { t: "copy"; text: string }
  | { t: "openLink"; href: string }
  | { t: "openFile"; path: string }
  | { t: "log"; line: string };

/** Name of the page-global the app calls through `injectJavaScript`. */
export const PAGE_RECEIVE_FUNCTION = "__muxflowReceive";

export function parseFromPageMessage(raw: string): FromPageMessage | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null || typeof (parsed as { t?: unknown }).t !== "string") return undefined;
  const message = parsed as Record<string, unknown>;
  switch (message.t) {
    case "ready":
      return { t: "ready" };
    case "size":
      if (typeof message.cols !== "number" || typeof message.rows !== "number") return undefined;
      return {
        t: "size",
        cols: message.cols,
        rows: message.rows,
        ...(typeof message.cellWidth === "number" ? { cellWidth: message.cellWidth } : {}),
        ...(typeof message.cellHeight === "number" ? { cellHeight: message.cellHeight } : {}),
      };
    case "written":
      return { t: "written", bytes: typeof message.bytes === "number" ? message.bytes : 0 };
    case "input":
      return typeof message.b64 === "string" && message.b64.length > 0 ? { t: "input", b64: message.b64 } : undefined;
    case "atTop":
      return { t: "atTop", above: typeof message.above === "number" && message.above >= 0 ? Math.floor(message.above) : 0 };
    case "scroll":
      if ((message.mode !== "normal" && message.mode !== "alternate") || typeof message.rows !== "number" || typeof message.durationMs !== "number") return undefined;
      return {
        t: "scroll",
        mode: message.mode,
        rows: Math.round(message.rows),
        durationMs: Math.max(0, Math.round(message.durationMs)),
        cancelled: message.cancelled === true,
      };
    case "copy":
      return typeof message.text === "string" && message.text.length > 0
        ? { t: "copy", text: message.text }
        : undefined;
    case "openLink":
      return typeof message.href === "string" && message.href.length > 0
        ? { t: "openLink", href: message.href }
        : undefined;
    case "openFile":
      return typeof message.path === "string" && message.path.length > 0
        ? { t: "openFile", path: message.path }
        : undefined;
    case "log":
      return { t: "log", line: typeof message.line === "string" ? message.line : "" };
    default:
      return undefined;
  }
}
