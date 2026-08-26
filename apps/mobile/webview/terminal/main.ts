// The terminal page (design doc §10.2): xterm.js fed by the app through the
// bridge in ../../src/features/terminal/bridgeMessages.ts. Bundled by
// scripts/build-webview.mjs into one self-contained HTML file. Nothing here
// talks to a network; `onData` is deliberately not wired (§10.2).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { PAGE_RECEIVE_FUNCTION, type FromPageMessage, type ToPageMessage } from "../../src/features/terminal/bridgeMessages";
import { computeGrid, NOMINAL_CELL, TERMINAL_FONT_SIZE_PX, TERMINAL_LINE_HEIGHT, sameGrid, type Grid } from "../../src/features/terminal/sizing";
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
let resizeTimer: ReturnType<typeof setTimeout> | undefined;

function root(): HTMLElement {
  return document.getElementById("terminal") as HTMLElement;
}

/** Re-measures the viewport, resizes xterm to whole cells, and reports the grid. */
function measure(force = false): void {
  if (!term || !fit) return;
  const proposed = fit.proposeDimensions();
  const el = root();
  const grid = proposed && proposed.cols > 0 && proposed.rows > 0
    ? { cols: Math.max(2, proposed.cols), rows: Math.max(1, proposed.rows) }
    : computeGrid({ width: el.clientWidth, height: el.clientHeight }, NOMINAL_CELL);
  if (!force && sameGrid(grid, lastGrid)) return;
  lastGrid = grid;
  if (term.cols !== grid.cols || term.rows !== grid.rows) term.resize(grid.cols, grid.rows);
  const cellWidth = el.clientWidth / grid.cols;
  const cellHeight = el.clientHeight / grid.rows;
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
    scrollback: 1000,
    scrollOnUserInput: false,
    disableStdin: true,
    theme: terminalTheme,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(root());
  measure(true);
  window.addEventListener("resize", scheduleMeasure);
  post({ t: "ready" });
}

function write(bytes: Uint8Array, reset: boolean): void {
  if (!term) return;
  if (reset) term.reset();
  term.write(bytes, () => post({ t: "written", bytes: bytes.byteLength }));
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
