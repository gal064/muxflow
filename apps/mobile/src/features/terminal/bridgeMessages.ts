// The JSON contract between the app and the terminal WebView page (§10.2).
// Imported by both sides so the two cannot drift.

/** RN → page. */
export type ToPageMessage =
  | { t: "init" }
  | { t: "seed"; b64: string }
  | { t: "out"; b64: string }
  | { t: "measure" };

/** page → RN. */
export type FromPageMessage =
  | { t: "ready" }
  | { t: "size"; cols: number; rows: number; cellWidth?: number; cellHeight?: number }
  | { t: "written"; bytes: number }
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
    case "log":
      return { t: "log", line: typeof message.line === "string" ? message.line : "" };
    default:
      return undefined;
  }
}
