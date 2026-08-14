import { Terminal, type IDisposable, type ILink } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SerializeAddon } from "@xterm/addon-serialize";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";

export interface TerminalSize {
  columns: number;
  rows: number;
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
  onResnapshotRequired?: (reason: string) => void;
}

export interface TerminalRenderer {
  open(element: HTMLElement): void;
  seed(bytes: Uint8Array, onRendered?: () => void, generation?: number): void;
  restore(serialized: string, onRendered?: () => void, generation?: number): void;
  write(bytes: Uint8Array, onRendered?: () => void, generation?: number): void;
  fit(): TerminalSize;
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
   * frame per record — measured at 23 frames (~383 ms) for a 4 KiB 24-record
   * repaint — and the keystroke echo queued behind it waited for all of them.
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
  #drainPromise?: Promise<DrainedTerminalSnapshot>;
  #disposed = false;

  constructor(options: TerminalRendererOptions = {}) {
    this.#options = options;
    this.#terminal = new Terminal({
      allowProposedApi: false,
      altClickMovesCursor: false,
      convertEol: false,
      cursorBlink: true,
      fontFamily: '"SFMono-Regular", "Cascadia Code", "JetBrains Mono", monospace',
      fontSize: 13,
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
      theme: {
        background: "#0d0f12",
        foreground: "#d8dee9",
        cursor: "#d8dee9",
        selectionBackground: "#526173aa",
        scrollbarSliderBackground: "#53617166",
        scrollbarSliderHoverBackground: "#71819799",
        scrollbarSliderActiveBackground: "#8da1b8bb",
      },
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
      (pending) => {
        const reason = `Terminal renderer queue exceeded its 8 MiB bound (${pending} bytes); requesting a fresh seed.`;
        this.#options.onDiagnostic?.(reason);
        this.#options.onResnapshotRequired?.(reason);
      },
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
    this.fit();
  }

  seed(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
    this.#newOutput = false;
    this.#scheduler.replace(bytes, true, this.#applied(generation, onRendered));
    this.#emitViewport();
  }

  restore(serialized: string, onRendered?: () => void, generation = 0): void {
    this.#newOutput = false;
    // A cached/resource restore is not a substitute for the scoped seed that
    // was requested after an output overflow.
    this.#scheduler.replace(new TextEncoder().encode(serialized), false, this.#applied(generation, onRendered));
    this.#emitViewport();
  }

  write(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
    if (!this.#atBottom()) this.#newOutput = true;
    this.#scheduler.enqueue(bytes, this.#applied(generation, onRendered));
    this.#emitViewport();
  }

  fit(): TerminalSize {
    this.#fit.fit();
    return { columns: this.#terminal.cols, rows: this.#terminal.rows };
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

  #applied(generation: number, onRendered?: () => void): () => void {
    return () => {
      if (Number.isSafeInteger(generation) && generation >= 0) this.#lastAppliedGeneration = generation;
      onRendered?.();
    };
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
