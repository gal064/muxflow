// The terminal page (design doc §10.2): xterm.js fed by the app through the
// bridge in ../../src/features/terminal/bridgeMessages.ts. Bundled by
// scripts/build-webview.mjs into one self-contained HTML file. Nothing here
// talks to a network; input events are captured only while synthesizing an
// alternate-screen wheel gesture (§10.2).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { PAGE_RECEIVE_FUNCTION, type FromPageMessage, type ToPageMessage } from "../../src/features/terminal/bridgeMessages";
import { computeGrid, NOMINAL_CELL, TERMINAL_FONT_SIZE_PX, TERMINAL_LINE_HEIGHT, sameGrid, type Grid } from "../../src/features/terminal/sizing";
import { TouchScrollController } from "../../src/features/terminal/touchScroll";
import { terminalTheme } from "../../src/ui/tokens";

declare global {
  interface Window {
    ReactNativeWebView?: { postMessage(message: string): void };
    [PAGE_RECEIVE_FUNCTION]?: (message: ToPageMessage) => void;
  }
}

const RESIZE_DEBOUNCE_MS = 100;

function post(message: FromPageMessage): void {
  window.ReactNativeWebView?.postMessage(JSON.stringify(message));
}

function decodeBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

let term: Terminal | undefined;
let fit: FitAddon | undefined;
let lastGrid: Grid | undefined;
let lastViewport: { width: number; height: number } | undefined;
let resizeTimer: ReturnType<typeof setTimeout> | undefined;
/** Last time the top of the buffer was reported (§7.6.1); one report per 2 s. */
let lastAtTopMs = 0;
const AT_TOP_THROTTLE_MS = 2_000;
/** Bounds synchronous xterm wheel encoding and the input batch to one frame's useful work. */
const MAX_ALTERNATE_WHEEL_EVENTS_PER_FRAME = 8;
const TOUCH_SCROLL_SENSITIVITY = 2;

function root(): HTMLElement {
  return document.getElementById("terminal") as HTMLElement;
}

/** Re-measures the viewport, resizes xterm to whole cells, and reports the grid. */
function measure(force = false): void {
  if (!term || !fit) return;
  const el = root();
  // Before layout the element is 0×0; a grid derived from that would resize
  // every window of the selected session to 2×1. Wait for the resize event.
  if (el.clientWidth < 1 || el.clientHeight < 1) return;
  const proposed = fit.proposeDimensions();
  const grid = proposed && proposed.cols > 0 && proposed.rows > 0
    ? { cols: Math.max(2, proposed.cols), rows: Math.max(1, proposed.rows) }
    : computeGrid({ width: el.clientWidth, height: el.clientHeight }, NOMINAL_CELL);
  const viewport = { width: el.clientWidth, height: el.clientHeight };
  const gridChanged = !sameGrid(grid, lastGrid);
  const viewportChanged = viewport.width !== lastViewport?.width || viewport.height !== lastViewport?.height;
  if (!force && !gridChanged && !viewportChanged) return;
  lastGrid = grid;
  lastViewport = viewport;
  if (gridChanged && (term.cols !== grid.cols || term.rows !== grid.rows)) term.resize(grid.cols, grid.rows);
  const cellWidth = viewport.width / grid.cols;
  const cellHeight = viewport.height / grid.rows;
  post({ t: "size", cols: grid.cols, rows: grid.rows, cellWidth, cellHeight });
}

function scheduleMeasure(): void {
  if (resizeTimer !== undefined) clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    resizeTimer = undefined;
    measure();
  }, RESIZE_DEBOUNCE_MS);
}

async function init(): Promise<void> {
  if (term) {
    measure(true);
    return;
  }
  // Measure with the real face: a fallback font would give a different cell.
  try {
    await document.fonts.load(`${TERMINAL_FONT_SIZE_PX}px "JetBrains Mono"`);
  } catch {
    // Fall through: xterm measures whatever is available.
  }
  term = new Terminal({
    allowProposedApi: false,
    convertEol: false,
    cursorBlink: true,
    cursorStyle: "block",
    fontFamily: '"JetBrains Mono", monospace',
    fontSize: TERMINAL_FONT_SIZE_PX,
    lineHeight: TERMINAL_LINE_HEIGHT,
    // The host clamps history at 10,000 rows and the controller stops paging
    // there (`HISTORY_MAX_SKIP_LINES`); a smaller buffer would trim its top
    // and throw the reader to the bottom on every splice past it.
    scrollback: 10_000,
    scrollOnUserInput: false,
    disableStdin: true,
    theme: terminalTheme,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(root());
  // xterm already knows how a wheel should reach the application in an
  // alternate screen: a mouse report when the TUI requested one, otherwise
  // an up/down cursor sequence. Keep stdin disabled at rest so the WebView
  // never becomes a second text-input surface; it is opened only around the
  // synthetic wheel dispatch below.
  let capturedWheelInput: string[] | undefined;
  term.onData((data) => capturedWheelInput?.push(data));
  term.onBinary((data) => capturedWheelInput?.push(data));
  // §7.6.1: reaching the top of the normal buffer asks the app for the
  // scrollback a screen-only seed left behind. The alternate screen has no
  // scrollback, and an empty buffer (nothing above the screen yet, or nothing
  // ever) still counts: `above` is 0 and the app decides.
  // Touch scrolling. xterm's own viewport is a scrollable div under the
  // screen layer, and inside this WebView a finger drag never reaches it (the
  // page is `overflow: hidden` and the screen layer takes the pointer), so
  // the drag is turned into `scrollLines` here. The controller batches moves
  // to one repaint per animation frame and adds the short decaying fling a
  // native scrolling surface would normally provide.
  const el = root();
  let touchX = 0;
  let touchY = 0;
  let gestureStartY: number | undefined;
  let gestureStartedAt = 0;
  let gestureMoved = false;
  let gestureMode: "normal" | "alternate" = "normal";
  const touchScroll = new TouchScrollController(
    (rows) => {
      if (!term) return;
      if (term.buffer.active.type === "normal") {
        term.scrollLines(rows);
        return;
      }
      const element = term.element;
      if (!element) return;
      const input: string[] = [];
      capturedWheelInput = input;
      term.options.disableStdin = false;
      try {
        // One line-mode wheel event per row preserves the controller's tuned
        // drag distance and fling while letting xterm select the active mouse
        // protocol and encoding. The events are collected into one bridge
        // message for this animation frame.
        const wheelEvents = Math.min(Math.abs(rows), MAX_ALTERNATE_WHEEL_EVENTS_PER_FRAME);
        for (let row = 0; row < wheelEvents; row += 1) {
          element.dispatchEvent(new WheelEvent("wheel", {
            bubbles: true,
            cancelable: true,
            clientX: touchX,
            clientY: touchY,
            deltaMode: WheelEvent.DOM_DELTA_LINE,
            deltaY: Math.sign(rows),
          }));
        }
      } finally {
        term.options.disableStdin = true;
        capturedWheelInput = undefined;
      }
      if (input.length > 0) post({ t: "input", b64: btoa(input.join("")) });
    },
    { request: (callback) => requestAnimationFrame(callback), cancel: (id) => cancelAnimationFrame(id) },
  );
  el.addEventListener("touchstart", (event) => {
    const touch = event.touches[0];
    if (touch) {
      touchX = touch.clientX;
      touchY = touch.clientY;
      gestureStartY = touch.clientY;
      gestureStartedAt = event.timeStamp;
      gestureMoved = false;
      gestureMode = term?.buffer.active.type ?? "normal";
      touchScroll.start(touch.clientY, event.timeStamp);
    }
  }, { passive: true });
  el.addEventListener("touchmove", (event) => {
    const touch = event.touches[0];
    if (!term || !touch) return;
    touchX = touch.clientX;
    touchY = touch.clientY;
    const cellHeight = lastGrid ? el.clientHeight / lastGrid.rows : NOMINAL_CELL.height;
    if (touchScroll.move(touch.clientY, event.timeStamp, cellHeight)) {
      gestureMoved = true;
      event.preventDefault();
    }
  }, { passive: false });
  const reportGesture = (timeMs: number, cancelled: boolean): void => {
    if (gestureStartY === undefined) return;
    if (gestureMoved) {
      const cellHeight = lastGrid ? el.clientHeight / lastGrid.rows : NOMINAL_CELL.height;
      post({
        t: "scroll",
        mode: gestureMode,
        rows: Math.round((gestureStartY - touchY) / cellHeight * TOUCH_SCROLL_SENSITIVITY),
        durationMs: Math.max(0, Math.round(timeMs - gestureStartedAt)),
        cancelled,
      });
    }
    gestureStartY = undefined;
    gestureMoved = false;
  };
  el.addEventListener("touchend", (event) => {
    touchScroll.end(event.timeStamp);
    reportGesture(event.timeStamp, false);
  }, { passive: true });
  el.addEventListener("touchcancel", (event) => {
    touchScroll.cancel();
    reportGesture(event.timeStamp, true);
  }, { passive: true });
  term.onScroll(() => {
    if (!term || term.buffer.active.type === "alternate") return;
    if (term.buffer.active.viewportY > 0) return;
    const now = Date.now();
    if (now - lastAtTopMs < AT_TOP_THROTTLE_MS) return;
    lastAtTopMs = now;
    post({ t: "atTop", above: Math.max(0, term.buffer.active.length - term.rows) });
  });
  measure(true);
  window.addEventListener("resize", scheduleMeasure);
  post({ t: "ready" });
}

/**
 * A reset ordered through xterm's write queue. `reset()` itself does not
 * touch the queue, and `write()` always parses later (and slices large writes
 * across frames), so a reset issued directly would land in the middle of
 * output still being parsed — those bytes would then be written into the
 * fresh buffer ahead of whatever follows the reset.
 */
function resetInOrder(then: () => void): void {
  if (!term) return;
  term.write("", () => {
    term?.reset();
    then();
  });
}

function write(bytes: Uint8Array, reset: boolean): void {
  if (!term) return;
  const go = () => term?.write(bytes, () => post({ t: "written", bytes: bytes.byteLength }));
  if (reset) resetInOrder(go);
  else go();
}

/**
 * §7.6.1: rebuild the buffer as history + screen. xterm has no prepend, so the
 * page resets, writes the history rows, scrolls all of them above the display
 * with one newline per screen row (the tail's seed begins with `ESC[2J ESC[H`,
 * which erases the display in place — a history row still on it would never
 * reach the scrollback), replays the tail, and puts the viewport back on the
 * first row the reader was already looking at: the history occupies exactly
 * its own row count above it.
 */
function splice(hist: Uint8Array, rowsAdded: number, tail: Uint8Array): void {
  if (!term) return;
  const pushOut = new TextEncoder().encode("\r\n".repeat(term.rows));
  resetInOrder(() => {
    if (!term) return;
    if (hist.byteLength > 0) term.write(hist);
    term.write(pushOut);
    term.write(tail, () => {
      if (!term) return;
      // The newest page sits above everything the reader already had, so the
      // row they were reading is now that many rows down from the top.
      term.scrollToLine(rowsAdded);
      post({ t: "written", bytes: hist.byteLength + tail.byteLength });
    });
  });
}

function receive(message: ToPageMessage): void {
  switch (message.t) {
    case "init":
      void init();
      return;
    case "measure":
      measure(true);
      return;
    case "seed":
      write(decodeBase64(message.b64), true);
      return;
    case "out":
      write(decodeBase64(message.b64), false);
      return;
    case "splice":
      splice(decodeBase64(message.hist), message.rowsAdded, decodeBase64(message.tail));
      return;
  }
}

window[PAGE_RECEIVE_FUNCTION] = (message) => {
  try {
    receive(message);
  } catch (error) {
    post({ t: "log", line: `page error: ${error instanceof Error ? error.message : String(error)}` });
  }
};

// react-native-webview's `postMessage` arrives as a `message` event on
// `document` on Android; supported as a second path to the injected call.
document.addEventListener("message", (event) => {
  const data = (event as MessageEvent).data;
  if (typeof data !== "string") return;
  try {
    window[PAGE_RECEIVE_FUNCTION]?.(JSON.parse(data) as ToPageMessage);
  } catch {
    // ignore malformed
  }
});
