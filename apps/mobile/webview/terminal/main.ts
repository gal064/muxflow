// The terminal page (design doc §10.2): xterm.js fed by the app through the
// bridge in ../../src/features/terminal/bridgeMessages.ts. Bundled by
// scripts/build-webview.mjs into one self-contained HTML file. Nothing here
// talks to a network; input events are captured only while synthesizing an
// alternate-screen wheel gesture (§10.2).

import { Terminal, type IBufferRange } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  captureTerminalSelection,
  cleanWrappedCommandSelection,
  terminalLinksForBufferLine,
} from "@muxflow/terminal-interactions";
import { PAGE_RECEIVE_FUNCTION, type FromPageMessage, type ToPageMessage } from "../../src/features/terminal/bridgeMessages";
import { computeGrid, NOMINAL_CELL, TERMINAL_FONT_SIZE_PX, TERMINAL_LINE_HEIGHT, sameGrid, type Grid } from "../../src/features/terminal/sizing";
import {
  selectionEdgeScrollDirection,
  selectionForTerminalRange,
  TerminalTouchIntent,
  TERMINAL_LONG_PRESS_MS,
} from "../../src/features/terminal/touchIntent";
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
const SELECTION_EDGE_PX = 24;
const SELECTION_EDGE_SCROLL_INTERVAL_MS = 60;
const SELECTION_HANDLE_KNOB_BELOW_ROW_PX = 14;

let selectingLink = false;

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
  if (term.cols !== grid.cols || term.rows !== grid.rows) term.resize(grid.cols, grid.rows);
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
  const hoveredLink = { range: undefined as IBufferRange | undefined };
  const currentHoveredLink = (): IBufferRange | undefined => hoveredLink.range;
  const sameRange = (left: IBufferRange | undefined, right: IBufferRange) => (
    left?.start.x === right.start.x && left.start.y === right.start.y
      && left.end.x === right.end.x && left.end.y === right.end.y
  );
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
    linkHandler: {
      activate: (_event, href) => {
        if (!selectingLink) post({ t: "openLink", href });
      },
      hover: (_event, _href, range) => {
        hoveredLink.range = range;
      },
      leave: (_event, _href, range) => {
        if (sameRange(hoveredLink.range, range)) hoveredLink.range = undefined;
      },
    },
    theme: terminalTheme,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(root());
  term.registerLinkProvider({
    provideLinks: (line, callback) => {
      if (!term) return callback(undefined);
      const links = terminalLinksForBufferLine(term.buffer.active, term.cols, line).map((link) => ({
        text: link.text,
        range: link.range,
        hover: () => {
          hoveredLink.range = link.range;
        },
        leave: () => {
          if (sameRange(hoveredLink.range, link.range)) hoveredLink.range = undefined;
        },
        activate: () => {
          if (selectingLink) return;
          if (link.kind === "web") post({ t: "openLink", href: link.text });
          else post({ t: "openFile", path: link.text });
        },
      }));
      callback(links.length > 0 ? links : undefined);
    },
  });
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
  let gestureMode: "normal" | "alternate" = "normal";
  let gestureRows = 0;
  const touchScroll = new TouchScrollController(
    (rows) => {
      if (!term) return;
      if (term.buffer.active.type === "normal") {
        term.scrollLines(rows);
        gestureRows += rows;
        return;
      }
      const element = term.element;
      if (!element) return;
      const input: string[] = [];
      const wheelEvents = Math.min(Math.abs(rows), MAX_ALTERNATE_WHEEL_EVENTS_PER_FRAME);
      capturedWheelInput = input;
      term.options.disableStdin = false;
      try {
        // One line-mode wheel event per row preserves the controller's tuned
        // drag distance and fling while letting xterm select the active mouse
        // protocol and encoding. The events are collected into one bridge
        // message for this animation frame.
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
      if (input.length > 0) {
        post({ t: "input", b64: btoa(input.join("")) });
        gestureRows += Math.sign(rows) * wheelEvents;
      }
    },
    { request: (callback) => requestAnimationFrame(callback), cancel: (id) => cancelAnimationFrame(id) },
    (summary) => {
      post({ t: "scroll", mode: gestureMode, rows: gestureRows, ...summary });
      gestureRows = 0;
    },
  );
  const touchIntent = new TerminalTouchIntent();
  let touchOrigin: { x: number; y: number; time: number } | undefined;
  let latestTouch: { x: number; y: number } | undefined;
  let longPressTimer: ReturnType<typeof setTimeout> | undefined;
  let hadSelectionAtTouchStart = false;

  const selectionStartHandle = document.createElement("button");
  selectionStartHandle.type = "button";
  selectionStartHandle.className = "muxflow-selection-control muxflow-selection-handle muxflow-selection-start";
  selectionStartHandle.setAttribute("aria-label", "Selection start");
  const selectionEndHandle = document.createElement("button");
  selectionEndHandle.type = "button";
  selectionEndHandle.className = "muxflow-selection-control muxflow-selection-handle muxflow-selection-end";
  selectionEndHandle.setAttribute("aria-label", "Selection end");
  const copyButton = document.createElement("button");
  copyButton.type = "button";
  copyButton.className = "muxflow-selection-control muxflow-copy-button";
  copyButton.textContent = "Copy";
  copyButton.setAttribute("aria-label", "Copy terminal selection");
  el.append(selectionStartHandle, selectionEndHandle, copyButton);

  const terminalScreenRect = () => (
    term?.element?.querySelector<HTMLElement>(".xterm-screen")?.getBoundingClientRect()
      ?? el.getBoundingClientRect()
  );
  const selectionMetrics = () => {
    if (!term) return undefined;
    const rect = terminalScreenRect();
    if (rect.width <= 0 || rect.height <= 0) return undefined;
    return {
      rect,
      cellWidth: rect.width / term.cols,
      cellHeight: rect.height / term.rows,
      viewportY: term.buffer.active.viewportY,
    };
  };
  const placeControl = (control: HTMLElement, left: number, top: number) => {
    control.style.left = `${Math.max(0, Math.min(el.clientWidth - control.offsetWidth, left))}px`;
    control.style.top = `${Math.max(0, Math.min(el.clientHeight - control.offsetHeight, top))}px`;
  };
  const placeCopyClearOfHandles = (preferredLeft: number, preferredTop: number) => {
    const gap = 4;
    const width = copyButton.offsetWidth;
    const height = copyButton.offsetHeight;
    const handles = [selectionStartHandle, selectionEndHandle].map((handle) => ({
      left: handle.offsetLeft,
      right: handle.offsetLeft + handle.offsetWidth,
      top: handle.offsetTop,
      bottom: handle.offsetTop + handle.offsetHeight,
    }));
    const topHandle = Math.min(...handles.map((handle) => handle.top));
    const bottomHandle = Math.max(...handles.map((handle) => handle.bottom));
    const leftCandidates = [preferredLeft, gap, el.clientWidth - width - gap];
    const topCandidates = [
      preferredTop,
      topHandle - height - gap,
      bottomHandle + gap,
      gap,
      el.clientHeight - height - gap,
    ];
    for (const rawTop of topCandidates) {
      const top = Math.max(0, Math.min(el.clientHeight - height, rawTop));
      for (const rawLeft of leftCandidates) {
        const left = Math.max(0, Math.min(el.clientWidth - width, rawLeft));
        const overlaps = handles.some((handle) => (
          left < handle.right + gap && left + width > handle.left - gap
            && top < handle.bottom + gap && top + height > handle.top - gap
        ));
        if (overlaps) continue;
        copyButton.style.left = `${left}px`;
        copyButton.style.top = `${top}px`;
        return;
      }
    }
    placeControl(copyButton, preferredLeft, preferredTop);
  };
  const updateSelectionUi = () => {
    if (!term) return;
    const position = term.getSelectionPosition();
    const metrics = selectionMetrics();
    const visible = Boolean(position && term.hasSelection() && metrics);
    for (const control of [selectionStartHandle, selectionEndHandle, copyButton]) {
      control.classList.toggle("visible", visible);
    }
    if (!position || !metrics || !visible) return;
    const localLeft = (x: number) => metrics.rect.left - el.getBoundingClientRect().left + x * metrics.cellWidth;
    const localTop = (y: number) => metrics.rect.top - el.getBoundingClientRect().top
      + (y - metrics.viewportY) * metrics.cellHeight;
    const startHandleTop = localTop(position.start.y + 1) - 8;
    const endHandleTop = localTop(position.end.y + 1) - 8;
    placeControl(selectionStartHandle, localLeft(position.start.x) - 22, startHandleTop);
    placeControl(selectionEndHandle, localLeft(position.end.x) - 22, endHandleTop);
    const copyGap = 4;
    const copyAbove = Math.min(selectionStartHandle.offsetTop, selectionEndHandle.offsetTop)
      - copyButton.offsetHeight - copyGap;
    const copyBelow = Math.max(
      selectionStartHandle.offsetTop + selectionStartHandle.offsetHeight,
      selectionEndHandle.offsetTop + selectionEndHandle.offsetHeight,
    ) + copyGap;
    const copyTop = copyAbove >= 0 ? copyAbove : copyBelow;
    placeCopyClearOfHandles((localLeft(position.start.x) + localLeft(position.end.x)) / 2 - 30, copyTop);
  };
  term.onSelectionChange(updateSelectionUi);

  const mouseEvent = (
    type: "mousemove" | "mousedown" | "mouseup",
    point: { x: number; y: number },
    detail: number,
    shiftKey = false,
  ) => {
    // xterm's linkifier listens on `.xterm-screen`; selection listens on the
    // outer terminal and receives these events through bubbling.
    term?.element?.querySelector<HTMLElement>(".xterm-screen")?.dispatchEvent(new MouseEvent(type, {
      bubbles: true,
      button: 0,
      buttons: type === "mouseup" ? 0 : 1,
      cancelable: true,
      clientX: point.x,
      clientY: point.y,
      detail,
      shiftKey,
      view: window,
    }));
  };
  const dispatchMouseGesture = (point: { x: number; y: number }, detail: number, selection: boolean) => {
    selectingLink = selection;
    try {
      mouseEvent("mousemove", point, detail);
      mouseEvent("mousedown", point, detail);
      mouseEvent("mouseup", point, detail);
    } finally {
      selectingLink = false;
    }
  };
  const selectLinkOrWord = (point: { x: number; y: number }) => {
    if (!term) return;
    selectingLink = true;
    try {
      // Hover asks both the built-in OSC 8 provider and the shared plain-text
      // provider for the link under this cell before selection is chosen. Keep
      // the existing range: xterm intentionally does not re-query providers
      // when the pointer is still over the same cell.
      mouseEvent("mousemove", point, 2, true);
      const range = currentHoveredLink();
      if (range) {
        const selection = selectionForTerminalRange(range, term.cols);
        if (selection) term.select(selection.column, selection.row, selection.length);
      } else {
        // Shift forces xterm's selection path only while a TUI has disabled
        // normal selection with mouse reporting. In the normal mode Shift
        // means "extend selection" and would suppress the double-click word.
        const forceSelection = term.modes.mouseTrackingMode !== "none";
        mouseEvent("mousedown", point, 2, forceSelection);
        mouseEvent("mouseup", point, 2, forceSelection);
      }
    } finally {
      selectingLink = false;
    }
  };
  const clearLongPress = () => {
    if (longPressTimer !== undefined) clearTimeout(longPressTimer);
    longPressTimer = undefined;
  };

  copyButton.addEventListener("touchstart", (event) => event.stopPropagation(), { passive: true });
  copyButton.addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (!term?.hasSelection()) return;
    const snapshot = captureTerminalSelection(term);
    const text = cleanWrappedCommandSelection(snapshot);
    if (text) post({ t: "copy", text });
  });

  type HandleDrag = { kind: "start" | "end"; fixed: number; grabX: number; grabY: number };
  let handleDrag: HandleDrag | undefined;
  let latestHandlePoint: Pick<Touch, "clientX" | "clientY"> | undefined;
  let edgeScrollDirection = 0;
  let edgeScrollTimer: ReturnType<typeof setInterval> | undefined;
  const linear = (x: number, y: number) => y * (term?.cols ?? 1) + x;
  const pointForTouch = (touch: Pick<Touch, "clientX" | "clientY">): number | undefined => {
    if (!term || !handleDrag) return undefined;
    const metrics = selectionMetrics();
    if (!metrics) return undefined;
    const endpointX = touch.clientX - handleDrag.grabX;
    const endpointRowBottom = touch.clientY - handleDrag.grabY - SELECTION_HANDLE_KNOB_BELOW_ROW_PX;
    const x = Math.max(0, Math.min(term.cols, Math.round((endpointX - metrics.rect.left) / metrics.cellWidth)));
    const viewportRow = Math.max(0, Math.min(
      term.rows - 1,
      Math.round((endpointRowBottom - metrics.rect.top) / metrics.cellHeight) - 1,
    ));
    const y = Math.max(0, Math.min(term.buffer.active.length - 1, metrics.viewportY + viewportRow));
    return linear(x, y);
  };
  const applyHandleDrag = (touch: Pick<Touch, "clientX" | "clientY">) => {
    if (!term || !handleDrag) return;
    const moving = pointForTouch(touch);
    if (moving === undefined) return;
    const start = Math.min(moving, handleDrag.fixed);
    const end = Math.max(moving, handleDrag.fixed);
    const boundedEnd = end === start ? end + 1 : end;
    term.select(start % term.cols, Math.floor(start / term.cols), boundedEnd - start);
  };
  const stopEdgeScroll = () => {
    if (edgeScrollTimer !== undefined) clearInterval(edgeScrollTimer);
    edgeScrollTimer = undefined;
    edgeScrollDirection = 0;
  };
  const updateEdgeScroll = (touch: Pick<Touch, "clientX" | "clientY">) => {
    if (!term || term.buffer.active.type !== "normal") {
      stopEdgeScroll();
      return;
    }
    const metrics = selectionMetrics();
    if (!metrics) return stopEdgeScroll();
    const direction = selectionEdgeScrollDirection(
      touch.clientY,
      metrics.rect.top,
      metrics.rect.bottom,
      SELECTION_EDGE_PX,
    );
    if (direction === edgeScrollDirection) return;
    stopEdgeScroll();
    if (direction === 0) return;
    edgeScrollDirection = direction;
    edgeScrollTimer = setInterval(() => {
      if (!term || !handleDrag || !latestHandlePoint || term.buffer.active.type !== "normal") {
        stopEdgeScroll();
        return;
      }
      term.scrollLines(edgeScrollDirection);
      applyHandleDrag(latestHandlePoint);
    }, SELECTION_EDGE_SCROLL_INTERVAL_MS);
  };
  const beginHandleDrag = (kind: HandleDrag["kind"], event: TouchEvent) => {
    event.preventDefault();
    event.stopPropagation();
    if (!term) return;
    const position = term.getSelectionPosition();
    const touch = event.touches[0];
    const metrics = selectionMetrics();
    if (!position || !touch || !metrics) return;
    const moving = kind === "start" ? position.start : position.end;
    const movingX = metrics.rect.left + moving.x * metrics.cellWidth;
    const movingY = metrics.rect.top + (moving.y - metrics.viewportY + 1) * metrics.cellHeight
      + SELECTION_HANDLE_KNOB_BELOW_ROW_PX;
    handleDrag = {
      kind,
      fixed: kind === "start"
        ? linear(position.end.x, position.end.y)
        : linear(position.start.x, position.start.y),
      grabX: touch.clientX - movingX,
      grabY: touch.clientY - movingY,
    };
    latestHandlePoint = { clientX: touch.clientX, clientY: touch.clientY };
  };
  selectionStartHandle.addEventListener("touchstart", (event) => beginHandleDrag("start", event), { passive: false });
  selectionEndHandle.addEventListener("touchstart", (event) => beginHandleDrag("end", event), { passive: false });
  document.addEventListener("touchmove", (event) => {
    if (!handleDrag) return;
    const touch = event.touches[0];
    if (!touch) return;
    event.preventDefault();
    event.stopPropagation();
    latestHandlePoint = { clientX: touch.clientX, clientY: touch.clientY };
    applyHandleDrag(touch);
    updateEdgeScroll(touch);
  }, { passive: false });
  const endHandleDrag = (event: TouchEvent) => {
    if (!handleDrag) return;
    event.preventDefault();
    event.stopPropagation();
    handleDrag = undefined;
    latestHandlePoint = undefined;
    stopEdgeScroll();
  };
  document.addEventListener("touchend", endHandleDrag, { passive: false });
  document.addEventListener("touchcancel", endHandleDrag, { passive: false });

  el.addEventListener("touchstart", (event) => {
    const touch = event.touches[0];
    const target = event.target;
    if (!touch || (target instanceof Element && target.closest(".muxflow-selection-control"))) return;
    // We synthesize exactly one xterm mouse gesture after classifying the
    // touch. Suppress the browser's compatibility mouse events so taps do not
    // activate a link twice and a long-press selection cannot activate it on
    // release.
    event.preventDefault();
    // A pending tap or long-press must target stationary content, so stop any
    // fling from the preceding scroll gesture before classifying this touch.
    touchScroll.cancel(event.timeStamp);
    touchX = touch.clientX;
    touchY = touch.clientY;
    latestTouch = { x: touch.clientX, y: touch.clientY };
    touchOrigin = { ...latestTouch, time: event.timeStamp };
    touchIntent.start(latestTouch);
    gestureMode = term?.buffer.active.type ?? "normal";
    gestureRows = 0;
    hadSelectionAtTouchStart = term?.hasSelection() ?? false;
    clearLongPress();
    longPressTimer = setTimeout(() => {
      longPressTimer = undefined;
      if (!latestTouch || !touchIntent.longPress()) return;
      selectLinkOrWord(latestTouch);
      updateSelectionUi();
    }, TERMINAL_LONG_PRESS_MS);
  }, { passive: false });
  el.addEventListener("touchmove", (event) => {
    const touch = event.touches[0];
    if (!term || !touch) return;
    touchX = touch.clientX;
    touchY = touch.clientY;
    latestTouch = { x: touch.clientX, y: touch.clientY };
    const decision = touchIntent.move(latestTouch);
    if (decision === "selection" || decision === "pending") {
      if (decision === "selection") event.preventDefault();
      return;
    }
    if (decision === "startScroll") {
      clearLongPress();
      if (term.hasSelection()) term.clearSelection();
      if (touchOrigin) touchScroll.start(touchOrigin.y, touchOrigin.time);
    }
    const cellHeight = lastGrid ? el.clientHeight / lastGrid.rows : NOMINAL_CELL.height;
    if (touchScroll.move(touch.clientY, event.timeStamp, cellHeight)) {
      event.preventDefault();
    }
  }, { passive: false });
  el.addEventListener("touchend", (event) => {
    clearLongPress();
    const decision = touchIntent.end();
    if (decision === "scroll") touchScroll.end(event.timeStamp);
    else if (decision === "tap" && latestTouch) {
      if (hadSelectionAtTouchStart) term?.clearSelection();
      else dispatchMouseGesture(latestTouch, 1, false);
    }
    touchOrigin = undefined;
    latestTouch = undefined;
  }, { passive: true });
  el.addEventListener("touchcancel", (event) => {
    clearLongPress();
    const decision = touchIntent.cancel();
    if (decision === "scroll") touchScroll.cancel(event.timeStamp);
    touchOrigin = undefined;
    latestTouch = undefined;
  }, { passive: true });
  term.onScroll(() => {
    updateSelectionUi();
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
