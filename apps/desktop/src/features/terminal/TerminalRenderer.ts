import { Terminal, type IDisposable, type ILink } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SerializeAddon } from "@xterm/addon-serialize";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { terminalFont, terminalTheme } from "./theme";

export interface TerminalSize {
  columns: number;
  rows: number;
}

/** A CSS-pixel box. The outer box of an element, borders and padding included. */
export interface PixelBox {
  width: number;
  height: number;
}

export type TerminalInput =
  | { kind: "text"; data: string }
  | { kind: "binary"; data: Uint8Array };

export interface TerminalViewportState {
  atBottom: boolean;
  newOutput: boolean;
}

export interface DrainedTerminalSnapshot {
  serialized: string;
  outputGeneration: number;
}

export interface TerminalRendererOptions {
  onDiagnostic?: (message: string | undefined) => void;
  onOpenLink?: (url: string) => void;
  /**
   * Asks the owner to fetch a fresh seed. Returning a promise lets the renderer
   * reopen its one-shot request latch when the request itself fails, so a pane
   * whose request never went out is not left permanently unable to ask again.
   */
  onResnapshotRequired?: (reason: string) => void | Promise<void>;
}

/** Why a grid was not applied, or the size that now governs the terminal. */
export type GridOutcome =
  | { kind: "applied"; size: TerminalSize }
  | { kind: "unchanged" }
  | { kind: "rejected"; reason: string };

export interface TerminalRenderer {
  open(element: HTMLElement): void;
  seed(bytes: Uint8Array, onRendered?: () => void, generation?: number): void;
  /**
   * Replaces the screen with a cached or host-owned snapshot, unless doing so
   * would erase newer output or stand in for a seed the pane owes the host.
   * Returns whether it was applied: a caller that follows a restore with a raw
   * tail must not write that tail onto a screen the restore did not lay down.
   */
  restore(
    serialized: string,
    onRendered?: () => void,
    generation?: number,
    throughGeneration?: number,
  ): boolean;
  write(bytes: Uint8Array, onRendered?: () => void, generation?: number): void;
  /** Measures the CSS box in cells. Does not resize the terminal. */
  measure(): TerminalSize | undefined;
  /**
   * What this terminal turns pixels into: one cell's size and the chrome a
   * terminal spends out of the box it is given. Values, not a callback — the
   * client-size computation is arithmetic over them, and a stale value is
   * visible where an unanswerable callback was not.
   */
  measurements(): TerminalMeasurements | undefined;
  /** Forces the grid tmux says this pane has, whatever the CSS box measured. */
  setGrid(size: TerminalSize): GridOutcome;
  focus(): void;
  blur(): void;
  onInput(listener: (input: TerminalInput) => void): () => void;
  onViewportChange(listener: (state: TerminalViewportState) => void): () => void;
  getSelection(): string;
  hasSelection(): boolean;
  paste(text: string): void;
  search(query: string, direction?: "next" | "previous"): boolean;
  clearSearch(): void;
  scrollToBottom(): void;
  serialize(): string;
  drainAndSerialize(): Promise<DrainedTerminalSnapshot>;
  disposeGpuRenderer(): void;
  dispose(): void;
}

type FrameRequest = (callback: FrameRequestCallback) => number;
type FrameCancel = (handle: number) => void;

/** xterm's default-feeling scroll animation, used while the pane is keeping up. */
const SMOOTH_SCROLL_DURATION_MS = 80;
/** Queue depth past which animating each scroll step is wasted work. */
const SMOOTH_SCROLL_SUSPEND_BYTES = 256 * 1024;

/**
 * Whether a cached or host-owned screen may replace what this terminal shows.
 *
 * A restore replaces the screen wholesale — including bytes still queued for
 * xterm — so an older one erases newer output and leaves the pane showing the
 * past: the stale-splice artifact in P12-U003.3. The comparison is against what
 * the terminal has been *given*, not what it has finished parsing. And after an overflow the pane owes the host a scoped seed; a
 * cached restore is not that seed, and silently doing nothing marks the pane
 * ready while it shows nothing. Both cases recover from the host instead.
 */
export function restoreDecision(
  throughGeneration: number,
  lastEnqueuedGeneration: number,
  overflowed: boolean,
): { kind: "apply" } | { kind: "reseed"; reason: string } {
  if (throughGeneration < lastEnqueuedGeneration) {
    return {
      kind: "reseed",
      reason: `A restore through generation ${throughGeneration} arrived for a pane that has already been given generation ${lastEnqueuedGeneration}; requesting a fresh seed.`,
    };
  }
  if (overflowed) {
    return {
      kind: "reseed",
      reason: "The pane overflowed its renderer queue; a cached restore cannot replace the seed it needs.",
    };
  }
  return { kind: "apply" };
}

/** Chrome a terminal spends out of the box it is given, in CSS pixels. */
export interface TerminalBoxChrome {
  horizontal: number;
  vertical: number;
  scrollbar: number;
}

/**
 * A terminal, plus the internal xterm does not expose: its render service's CSS
 * cell size. The dependency is in the signature rather than inside a cast so
 * that "this reads xterm internals" is visible to the next reader and to the
 * next upgrade.
 */
export type MeasurableTerminal = Pick<Terminal, "options"> & {
  _core?: { _renderService?: { dimensions?: { css?: { cell?: Partial<PixelBox> } } } };
};

/** Everything needed to turn a pixel box into a terminal grid. */
export interface TerminalMeasurements {
  cell: PixelBox;
  chrome: TerminalBoxChrome;
}

/**
 * Reads a terminal's cell size and chrome from the DOM and from xterm's own
 * render service — the same places `FitAddon.proposeDimensions` reads them.
 *
 * Exported and parameterised so the reading, not a reimplementation of it, is
 * what the tests exercise: `measureBox.test.ts` runs this against a real
 * `Terminal` and asserts it agrees with `FitAddon`. Everything is
 * optional-chained: if a future xterm moves the render service, this reports
 * nothing and the app asks tmux for nothing, which is the safe outcome.
 */
export function terminalMeasurements(
  terminal: MeasurableTerminal,
  host: Element,
  element: Element,
): TerminalMeasurements | undefined {
  const cell = terminal._core?._renderService?.dimensions?.css?.cell;
  if (!cell?.width || !cell.height) return undefined;
  const hostStyle = window.getComputedStyle(host);
  const terminalStyle = window.getComputedStyle(element);
  return {
    cell: { width: cell.width, height: cell.height },
    chrome: {
      horizontal: edges(hostStyle, "left", "right") + edges(terminalStyle, "left", "right"),
      vertical: edges(hostStyle, "top", "bottom") + edges(terminalStyle, "top", "bottom"),
      // xterm reserves this on the right whenever there is scrollback, and
      // FitAddon subtracts it before dividing; a terminal sized without it
      // renders its last columns under the scrollbar.
      scrollbar: terminal.options.scrollback === 0 ? 0 : terminal.options.overviewRuler?.width || 14,
    },
  };
}

/**
 * Cells that fit a pixel box, given one cell's size and the terminal's own
 * chrome. Extracted from `measureBox` so the arithmetic that decides how big a
 * tmux client to ask for is testable without a DOM: it is the arithmetic
 * `FitAddon.proposeDimensions` performs, with an explicit box.
 *
 * Floors, never rounds. Half a cell of terminal is not a cell of terminal, and
 * rounding up asks tmux for a grid the surface cannot show — which is how a
 * pane ends up with its bottom row cut off.
 */
export function cellsForBox(
  box: PixelBox,
  cell: PixelBox,
  chrome: TerminalBoxChrome,
): TerminalSize | undefined {
  if (!(cell.width > 0) || !(cell.height > 0)) return undefined;
  const width = box.width - chrome.horizontal - chrome.scrollbar;
  const height = box.height - chrome.vertical;
  if (!(width > 0) || !(height > 0)) return undefined;
  return { columns: Math.floor(width / cell.width), rows: Math.floor(height / cell.height) };
}

/**
 * Padding plus border an element spends on the named sides, in CSS pixels.
 *
 * A border with no style spends nothing. Browsers already compute its width to
 * `0px`, so this guard changes nothing in the app; jsdom reports the initial
 * `medium` (16 px) instead, and without it `measureBox.test.ts` would be
 * asserting against 64 px of border that does not exist anywhere.
 */
function edges(style: CSSStyleDeclaration, ...sides: Array<"top" | "bottom" | "left" | "right">): number {
  return sides.reduce((total, side) => {
    const padding = pixels(style.getPropertyValue(`padding-${side}`));
    const invisible = ["none", "hidden", ""].includes(style.getPropertyValue(`border-${side}-style`));
    return total + padding + (invisible ? 0 : pixels(style.getPropertyValue(`border-${side}-width`)));
  }, 0);
}

function pixels(value: string): number {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function joinChunks(pieces: Uint8Array[], length: number): Uint8Array {
  if (pieces.length === 1) return pieces[0];
  const joined = new Uint8Array(length);
  let offset = 0;
  for (const piece of pieces) {
    joined.set(piece, offset);
    offset += piece.byteLength;
  }
  return joined;
}

/** A byte-preserving queue bounded across both JS and xterm's async parser. */
export class TerminalWriteScheduler {
  readonly #queue: Array<{ bytes: Uint8Array; onRendered?: () => void }> = [];
  #frame?: number;
  #disposed = false;
  #pendingBytes = 0;
  #inFlightBytes = 0;
  #overflowed = false;
  #accepting = true;
  #immediateWriteUsed = false;
  #immediateResetFrame?: number;
  readonly #drainWaiters = new Set<() => void>();

  constructor(
    readonly writeChunk: (chunk: Uint8Array, done: () => void) => void,
    readonly requestFrame: FrameRequest = (callback) => window.requestAnimationFrame(callback),
    readonly cancelFrame: FrameCancel = (handle) => window.cancelAnimationFrame(handle),
    readonly maxBytesPerFrame = 256 * 1024,
    readonly maxPendingBytes = 8 * 1024 * 1024,
    readonly onPendingBytes?: (bytes: number) => void,
    readonly onOverflow?: (pendingBytes: number) => void,
  ) {}

  enqueue(bytes: Uint8Array, onRendered?: () => void): boolean {
    if (this.#disposed || !this.#accepting || this.#overflowed) return false;
    if (bytes.byteLength === 0) {
      onRendered?.();
      return true;
    }
    if (this.#pendingBytes + bytes.byteLength > this.maxPendingBytes) {
      const attemptedBytes = this.#pendingBytes + bytes.byteLength;
      this.#dropQueued();
      this.#overflowed = true;
      this.onOverflow?.(attemptedBytes);
      return false;
    }
    this.#queue.push({ bytes: bytes.slice(), onRendered });
    this.#pendingBytes += bytes.byteLength;
    this.onPendingBytes?.(this.#pendingBytes);
    this.#schedule();
    return true;
  }

  replace(bytes: Uint8Array, recoverOverflow = true, onRendered?: () => void): void {
    if (this.#disposed || (this.#overflowed && !recoverOverflow)) return;
    this.#dropQueued();
    this.#overflowed = false;
    const resetAndBytes = new Uint8Array(bytes.byteLength + 2);
    resetAndBytes.set([0x1b, 0x63]);
    resetAndBytes.set(bytes, 2);
    this.enqueue(resetAndBytes, onRendered);
  }

  clear(): void {
    this.#dropQueued();
    this.#overflowed = false;
  }

  #dropQueued(): void {
    this.#queue.length = 0;
    this.#pendingBytes = this.#inFlightBytes;
    if (this.#frame !== undefined) this.cancelFrame(this.#frame);
    this.#frame = undefined;
    this.onPendingBytes?.(this.#pendingBytes);
  }

  dispose(): void {
    this.clear();
    this.#disposed = true;
    this.#accepting = false;
    if (this.#immediateResetFrame !== undefined) this.cancelFrame(this.#immediateResetFrame);
    this.#immediateResetFrame = undefined;
    // Bytes already inside xterm's parser are unrecoverable once the terminal
    // is disposed: nothing will call their completion. Anyone awaiting the
    // drain has to be released anyway, or the next reveal of this pane — which
    // waits on that promise — never happens.
    this.#pendingBytes = 0;
    this.#inFlightBytes = 0;
    this.#resolveDrainWaiters();
  }

  /** Stop admitting writes and resolve only after queued and in-flight bytes reach xterm. */
  sealAndDrain(): Promise<void> {
    this.#accepting = false;
    if (this.#pendingBytes === 0) return Promise.resolve();
    return new Promise((resolve) => this.#drainWaiters.add(resolve));
  }

  get pendingBytes(): number {
    return this.#pendingBytes;
  }

  get overflowed(): boolean {
    return this.#overflowed;
  }

  #schedule(): void {
    if (this.#disposed || this.#inFlightBytes || this.#frame !== undefined || this.#queue.length === 0) return;
    // Idle fast path. An echoed keystroke is a few bytes arriving into an empty
    // queue, and waiting for the next animation frame quantises it by up to a
    // whole frame — on a 60 Hz display that is most of the local keystroke
    // budget spent doing nothing. At most one write per frame skips the wait,
    // so a flood still gets frame-paced exactly as before.
    if (this.#queue.length === 1 && !this.#immediateWriteUsed) {
      this.#immediateWriteUsed = true;
      this.#armImmediateWriteReset();
      this.#flush();
      return;
    }
    this.#frame = this.requestFrame(() => this.#flush());
  }

  #armImmediateWriteReset(): void {
    if (this.#immediateResetFrame !== undefined) return;
    this.#immediateResetFrame = this.requestFrame(() => {
      this.#immediateResetFrame = undefined;
      this.#immediateWriteUsed = false;
      this.#schedule();
    });
  }

  /**
   * Hands xterm one frame's worth of bytes: as many queued events as the byte
   * budget covers, coalesced into a single write.
   *
   * Draining one *event* per frame was the renderer half of P12-U002. tmux
   * splits an agent-TUI repaint across many output records, so a repaint cost a
   * frame per record: 23 frames for a 4 KiB 24-record repaint, counted
   * deterministically against an injected frame clock in
   * `TerminalRenderer.test.ts` (a modelled frame count, not a wall-clock
   * measurement) — and the keystroke echo queued behind it waited for all.
   * The budget is bytes, so a flood is paced exactly as before, and coalescing
   * keeps the "one write outstanding in xterm at a time" bound that the pending
   * accounting, the overflow bound and the drain all rest on.
   */
  #flush(): void {
    this.#frame = undefined;
    if (this.#disposed || this.#inFlightBytes || this.#queue.length === 0) return;
    const pieces: Uint8Array[] = [];
    const rendered: Array<() => void> = [];
    let length = 0;
    while (length < this.maxBytesPerFrame && this.#queue.length > 0) {
      const first = this.#queue[0];
      const take = Math.min(first.bytes.byteLength, this.maxBytesPerFrame - length);
      pieces.push(first.bytes.subarray(0, take));
      length += take;
      if (take === first.bytes.byteLength) {
        this.#queue.shift();
        // A partially written event has not reached xterm yet, so its
        // completion belongs to the frame that finishes it.
        if (first.onRendered) rendered.push(first.onRendered);
      } else {
        first.bytes = first.bytes.subarray(take);
      }
    }
    const chunk = joinChunks(pieces, length);
    this.#inFlightBytes = length;
    let completed = false;
    const done = () => {
      if (completed) return;
      completed = true;
      this.#pendingBytes -= this.#inFlightBytes;
      this.#inFlightBytes = 0;
      this.onPendingBytes?.(this.#pendingBytes);
      for (const onRendered of rendered) onRendered();
      this.#resolveDrainWaiters();
      this.#schedule();
    };
    try {
      this.writeChunk(chunk, done);
    } catch {
      done();
      this.#dropQueued();
      this.#overflowed = true;
      this.onOverflow?.(this.#pendingBytes);
    }
  }


  #resolveDrainWaiters(): void {
    if (this.#pendingBytes !== 0) return;
    for (const resolve of this.#drainWaiters) resolve();
    this.#drainWaiters.clear();
  }
}

export class XtermRenderer implements TerminalRenderer {
  readonly #terminal: Terminal;
  readonly #fit = new FitAddon();
  readonly #serialize = new SerializeAddon();
  readonly #search = new SearchAddon({ highlightLimit: 1_000 });
  readonly #viewportListeners = new Set<(state: TerminalViewportState) => void>();
  readonly #disposables: IDisposable[] = [];
  readonly #scheduler: TerminalWriteScheduler;
  readonly #options: TerminalRendererOptions;
  #webgl?: WebglAddon;
  #newOutput = false;
  #lastViewport?: TerminalViewportState;
  #lastAppliedGeneration = 0;
  /// What this terminal has been *given*, which runs ahead of what it has
  /// applied. A restore drops the queue, so admitting one has to be judged
  /// against the queued bytes it would discard, not only the parsed ones.
  #lastEnqueuedGeneration = 0;
  #seedRequested = false;
  #drainPromise?: Promise<DrainedTerminalSnapshot>;
  #disposed = false;

  constructor(options: TerminalRendererOptions = {}) {
    this.#options = options;
    // Font and palette both come from `tokens.css` (see ./theme.ts), so the
    // terminal is a Ghostty surface by derivation rather than by a second set
    // of literals that drifted from the chrome.
    const font = terminalFont();
    this.#terminal = new Terminal({
      allowProposedApi: false,
      altClickMovesCursor: false,
      convertEol: false,
      cursorBlink: true,
      cursorStyle: "block",
      fontFamily: font.fontFamily,
      fontSize: font.fontSize,
      lineHeight: font.lineHeight,
      ignoreBracketedPasteMode: false,
      macOptionClickForcesSelection: true,
      rightClickSelectsWord: true,
      // Off deliberately (P12-U002/U003). xterm's screen-reader mode allocates
      // a string and dispatches an emitter event for every printed codepoint
      // and rewrites a DOM mirror of every row on every render, which an agent
      // TUI repainting at 1 Hz pays thousands of times a second; its mirror also
      // sits over the WebGL canvas with an un-overridden `::selection`
      // background, which is what painted highlight rectangles at stale
      // positions. The pane's own AX label and role, keyboard operability and
      // xterm's input textarea are unaffected. Making terminal *content*
      // readable to a screen reader again belongs behind a user setting; there
      // is no preferences surface to hang one on yet.
      screenReaderMode: false,
      scrollback: 10_000,
      scrollOnUserInput: true,
      smoothScrollDuration: SMOOTH_SCROLL_DURATION_MS,
      windowOptions: {
        getCellSizePixels: true,
        getWinSizeChars: true,
        getWinSizePixels: true,
      },
      linkHandler: {
        activate: (_event, url) => this.#activateLink(url),
      },
      theme: terminalTheme(),
    });
    this.#terminal.loadAddon(this.#fit);
    this.#terminal.loadAddon(this.#serialize);
    this.#terminal.loadAddon(this.#search);
    this.#scheduler = new TerminalWriteScheduler(
      (chunk, done) => this.#terminal.write(chunk, done),
      (callback) => window.requestAnimationFrame(callback),
      (handle) => window.cancelAnimationFrame(handle),
      256 * 1024,
      8 * 1024 * 1024,
      (pending) => {
        this.#applyScrollSmoothingForLoad(pending);
        if (pending > 4 * 1024 * 1024) this.#options.onDiagnostic?.("Terminal output is catching up…");
        else if (pending === 0 && this.#webgl) this.#options.onDiagnostic?.(undefined);
      },
      (pending) => this.#requestSeed(
        `Terminal renderer queue exceeded its 8 MiB bound (${pending} bytes); requesting a fresh seed.`,
      ),
    );
    this.#disposables.push(this.#terminal.onScroll(() => {
      if (this.#atBottom()) this.#newOutput = false;
      this.#emitViewport();
    }));
    this.#disposables.push(this.#terminal.registerLinkProvider({
      provideLinks: (line, callback) => callback(this.#linksForLine(line)),
    }));
  }

  open(element: HTMLElement): void {
    this.#terminal.open(element);
    this.#mountWebgl();
  }

  seed(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
    this.#newOutput = false;
    // A seed is the whole screen, so it also re-bases the applied-generation
    // watermark. Without that reset, a seed from a new terminal epoch — whose
    // generations restart at 1 — would sit below a watermark the previous epoch
    // left behind, and every later restore and hide checkpoint would be
    // measured against a number from a stream that no longer exists.
    this.#lastAppliedGeneration = 0;
    this.#lastEnqueuedGeneration = 0;
    // The seed is the recovery this pane may have asked for; the next refusal
    // is allowed to ask again.
    this.#seedRequested = false;
    this.#scheduler.replace(bytes, true, this.#enqueued(generation, onRendered));
    this.#emitViewport();
  }

  restore(
    serialized: string,
    onRendered?: () => void,
    generation = 0,
    throughGeneration = generation,
  ): boolean {
    const decision = restoreDecision(throughGeneration, this.#lastEnqueuedGeneration, this.#scheduler.overflowed);
    if (decision.kind === "reseed") {
      this.#requestSeed(decision.reason);
      return false;
    }
    this.#newOutput = false;
    this.#scheduler.replace(new TextEncoder().encode(serialized), false, this.#enqueued(generation, onRendered));
    this.#emitViewport();
    return true;
  }

  write(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
    if (!this.#atBottom()) this.#newOutput = true;
    if (!this.#scheduler.enqueue(bytes, this.#enqueued(generation, onRendered)) && this.#scheduler.overflowed) {
      // Only an overflow refusal means bytes were lost. The scheduler also
      // refuses when it is disposed or sealed for the hide drain, and asking
      // for a seed then would be recovery for a pane that is going away.
      this.#requestSeed("Terminal output could not be queued for this pane; requesting a fresh seed.");
    }
    this.#emitViewport();
  }

  /**
   * Measures the CSS box in cells without touching the terminal.
   *
   * Deliberately propose-only: `setGrid` is the single writer of cols/rows, so
   * a measurement can never reflow the buffer to the box and back to tmux's
   * grid within one observer callback — a shrink-then-grow reflow is not
   * lossless in xterm.
   */
  measure(): TerminalSize | undefined {
    const proposed = this.#fit.proposeDimensions();
    if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows)) return undefined;
    return { columns: proposed.cols, rows: proposed.rows };
  }

  /**
   * The cell size and chrome this terminal would spend in any host element.
   *
   * Both come from where `FitAddon.proposeDimensions` takes them: the render
   * service's CSS cell size, the host element's own padding and border, the
   * terminal element's padding, and xterm's scrollbar allowance. Reporting
   * them as values is what lets the tmux client size be computed from the
   * tiled surface rather than from any pane (P12-U006) — the numbers describe
   * a terminal, not this pane's box.
   */
  measurements(): TerminalMeasurements | undefined {
    const element = this.#terminal.element;
    // The element xterm was opened into. FitAddon measures this element's
    // content box and then subtracts the terminal's own padding, so a caller
    // holding an *outer* box has to give up both.
    const host = element?.parentElement;
    if (!element || !host) return undefined;
    return terminalMeasurements(this.#terminal as MeasurableTerminal, host, element);
  }

  /**
   * tmux owns this pane's grid: the program inside it drew for tmux's cols and
   * rows, and a cursor-addressed frame rendered against any other grid puts
   * text on the wrong lines (P12-U003.1). The CSS box can only ever approximate
   * that grid — tmux splits 100 columns into 50 and 49 with a divider column,
   * while the box is a percentage that rounds per pane — so the measurement
   * decides how big a *client* to ask tmux for, and this decides what the
   * terminal actually renders at.
   */
  setGrid({ columns, rows }: TerminalSize): GridOutcome {
    if (!Number.isInteger(columns) || !Number.isInteger(rows) || columns < 2 || rows < 2) {
      return { kind: "rejected", reason: `${columns}x${rows} is not a usable terminal grid` };
    }
    if (this.#terminal.cols === columns && this.#terminal.rows === rows) return { kind: "unchanged" };
    this.#terminal.resize(columns, rows);
    return { kind: "applied", size: { columns, rows } };
  }

  focus(): void {
    this.#terminal.focus();
  }

  blur(): void {
    this.#terminal.blur();
  }

  onInput(listener: (input: TerminalInput) => void): () => void {
    const data = this.#terminal.onData((value) => listener({ kind: "text", data: value }));
    const binary = this.#terminal.onBinary((value) => {
      listener({ kind: "binary", data: Uint8Array.from(value, (character) => character.charCodeAt(0) & 0xff) });
    });
    return () => {
      data.dispose();
      binary.dispose();
    };
  }

  onViewportChange(listener: (state: TerminalViewportState) => void): () => void {
    this.#viewportListeners.add(listener);
    listener({ atBottom: this.#atBottom(), newOutput: this.#newOutput });
    return () => this.#viewportListeners.delete(listener);
  }

  getSelection(): string {
    return this.#terminal.getSelection();
  }

  hasSelection(): boolean {
    return this.#terminal.hasSelection();
  }

  paste(text: string): void {
    this.#terminal.paste(text);
  }

  search(query: string, direction: "next" | "previous" = "next"): boolean {
    if (!query) return false;
    const options = {
      decorations: {
        matchBackground: "#394a5e",
        matchOverviewRuler: "#6f8dab",
        activeMatchBackground: "#8a6d32",
        activeMatchColorOverviewRuler: "#d6a84d",
      },
    };
    return direction === "next" ? this.#search.findNext(query, options) : this.#search.findPrevious(query, options);
  }

  clearSearch(): void {
    this.#search.clearDecorations();
  }

  scrollToBottom(): void {
    this.#terminal.scrollToBottom();
    this.#newOutput = false;
    this.#emitViewport();
  }

  serialize(): string {
    return this.#serialize.serialize({ scrollback: 10_000 });
  }

  drainAndSerialize(): Promise<DrainedTerminalSnapshot> {
    this.#drainPromise ??= this.#scheduler.sealAndDrain().then(() => ({
      serialized: this.serialize(),
      outputGeneration: this.#lastAppliedGeneration,
    }));
    return this.#drainPromise;
  }

  disposeGpuRenderer(): void {
    this.#webgl?.dispose();
    this.#webgl = undefined;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#scheduler.dispose();
    this.disposeGpuRenderer();
    for (const disposable of this.#disposables) disposable.dispose();
    this.#terminal.dispose();
  }

  #atBottom(): boolean {
    const buffer = this.#terminal.buffer.active;
    return buffer.viewportY >= buffer.baseY;
  }

  /// Records what the terminal was handed, and returns the completion that
  /// records what it applied.
  #enqueued(generation: number, onRendered?: () => void): () => void {
    if (Number.isSafeInteger(generation) && generation > this.#lastEnqueuedGeneration) {
      this.#lastEnqueuedGeneration = generation;
    }
    return this.#applied(generation, onRendered);
  }

  #applied(generation: number, onRendered?: () => void): () => void {
    return () => {
      // Monotonic: this is the cutoff the hide handoff hands the host, and a
      // restore that rewound it made the next checkpoint claim bytes the host
      // would then never resend.
      if (Number.isSafeInteger(generation) && generation > this.#lastAppliedGeneration) {
        this.#lastAppliedGeneration = generation;
      }
      onRendered?.();
    };
  }

  /**
   * The diagnostic is never suppressed — a refused write or a refused restore
   * is exactly the silence P12-U003.7 was about. Only the *request* is deduped,
   * so one overflow cannot become a reseed storm; a request that fails reopens
   * the latch here rather than at the caller, and a seed landing clears it.
   */
  #requestSeed(reason: string): void {
    this.#options.onDiagnostic?.(reason);
    if (this.#seedRequested) return;
    this.#seedRequested = true;
    void Promise.resolve(this.#options.onResnapshotRequired?.(reason)).catch(() => {
      this.#seedRequested = false;
    });
  }

  /**
   * Notifies only on an actual change. This fired once per output chunk per
   * pane, and each notification is a React `setState` with a fresh object, so
   * a busy pane re-rendered its surface for every chunk it received while
   * saying nothing new.
   */
  #emitViewport(): void {
    const atBottom = this.#atBottom();
    if (this.#lastViewport?.atBottom === atBottom && this.#lastViewport.newOutput === this.#newOutput) return;
    const state = { atBottom, newOutput: this.#newOutput };
    this.#lastViewport = state;
    for (const listener of this.#viewportListeners) listener(state);
  }

  /**
   * Smooth scrolling animates each scroll step, which is pleasant when the user
   * scrolls and pure overhead when output is arriving faster than frames. It is
   * disabled while the queue is backed up and restored when it drains, so the
   * resting behaviour is unchanged.
   */
  #applyScrollSmoothingForLoad(pendingBytes: number): void {
    const smoothing = pendingBytes > SMOOTH_SCROLL_SUSPEND_BYTES ? 0 : SMOOTH_SCROLL_DURATION_MS;
    if (this.#terminal.options.smoothScrollDuration === smoothing) return;
    this.#terminal.options.smoothScrollDuration = smoothing;
  }

  #mountWebgl(): void {
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl.dispose();
        if (this.#webgl === webgl) this.#webgl = undefined;
        this.#options.onDiagnostic?.("WebGL context lost; using the canvas renderer.");
      });
      this.#terminal.loadAddon(webgl);
      this.#webgl = webgl;
      this.#options.onDiagnostic?.(undefined);
    } catch (error) {
      this.#webgl = undefined;
      this.#options.onDiagnostic?.("WebGL unavailable; using the canvas renderer.");
      console.warn("WebGL terminal renderer unavailable; using canvas renderer", error);
    }
  }

  #activateLink(value: string): void {
    try {
      const url = new URL(value);
      if (url.protocol !== "http:" && url.protocol !== "https:") return;
      this.#options.onOpenLink?.(url.href);
    } catch {
      // Invalid and non-HTTP OSC links remain inert.
    }
  }

  #linksForLine(bufferLineNumber: number): ILink[] | undefined {
    const line = this.#terminal.buffer.active.getLine(bufferLineNumber - 1)?.translateToString(true);
    if (!line) return undefined;
    const links: ILink[] = [];
    const pattern = /https?:\/\/[^\s<>"']+/gu;
    for (const match of line.matchAll(pattern)) {
      const text = match[0];
      const start = (match.index ?? 0) + 1;
      links.push({
        text,
        range: {
          start: { x: start, y: bufferLineNumber },
          end: { x: start + text.length, y: bufferLineNumber },
        },
        activate: () => this.#activateLink(text),
      });
    }
    return links.length ? links : undefined;
  }
}
