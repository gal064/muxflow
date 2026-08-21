import { Terminal, type IDisposable, type ILink } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SerializeAddon } from "@xterm/addon-serialize";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { terminalScreenReaderMode } from "./accessibilityPreference";
import { installAtlasFontSmoothing } from "./atlasFontSmoothing";
import { watchAtlasStaleness } from "./atlasStaleProbe";
import { GHOSTTY_TEXT_OPTIONS, searchDecorations, terminalFacesPending, terminalFacesReady, terminalFont, terminalTheme, WEBGL_CELL_SPACING } from "./theme";
import {
  deviceSafeLineHeight,
  terminalMeasurements,
  type MeasurableTerminal,
  type TerminalMeasurements,
  type TerminalSize,
} from "./cellMetrics";
import type { OwnedTerminalBytes } from "./TerminalBytes";
import { TerminalGenerationWatermark } from "./TerminalGenerationWatermark";
import { TerminalWriteScheduler } from "./TerminalWriteScheduler";
import { settleWithin } from "./timeBound";
import { recordPerfCounter } from "../../perf/probe";
import { recordIncident } from "../../diagnostics/incidents";
import { isTerminalFileLinkActivation, terminalFileLinkCellRange, terminalFileLinks } from "./terminalFilePaths";
import { installOsc52ClipboardWrite } from "./osc52Clipboard";

// Re-exported so the renderer stays the one import site for a pane's metrics.
export type { PixelBox, TerminalBoxChrome, TerminalMeasurements, TerminalSize } from "./cellMetrics";
export { cellsForBox, deviceSafeLineHeight, terminalMeasurements } from "./cellMetrics";

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
  /**
   * Which pane this renderer draws, for the incident journal only. The renderer
   * carries no identity of its own, and a `render.webglFallback` record that
   * cannot say *which* pane fell back is not worth reading.
   */
  paneId?: string;
  onDiagnostic?: (message: string | undefined) => void;
  onOpenLink?: (url: string) => void;
  onOpenFilePath?: (path: string) => void;
  /**
   * Asks the owner to fetch a fresh seed. Returning a promise lets the renderer
   * reopen its one-shot request latch when the request itself fails, so a pane
   * whose request never went out is not left permanently unable to ask again.
   */
  onResnapshotRequired?: (reason: string) => void | Promise<void>;
  /**
   * How long `drainAndSerialize` waits for xterm before giving up on it. Exposed
   * for tests; every production caller takes the default.
   */
  drainTimeoutMs?: number;
  /** Receives write-only OSC 52 clipboard requests emitted by terminal apps. */
  onClipboardWrite?: (text: string) => void | Promise<void>;
  onClipboardWriteError?: (error: unknown) => void;
}

/**
 * A hide drain that xterm never completes used to be permanent: the memoized
 * promise never settled, the pane's handoff never resolved, and the reveal that
 * waits behind that handoff never ran again.
 */
export const DEFAULT_DRAIN_TIMEOUT_MS = 2_000;

/** Why a grid was not applied, or the size that now governs the terminal. */
export type GridOutcome =
  | { kind: "applied"; size: TerminalSize }
  | { kind: "unchanged" }
  | { kind: "rejected"; reason: string };

export interface TerminalRenderer {
  open(element: HTMLElement): void;
  seed(bytes: OwnedTerminalBytes, onRendered?: () => void, generation?: number): void;
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
  /**
   * Queues output for xterm. Returns whether the record was accepted: a
   * refusal means `onRendered` will never run, so a caller that hangs an
   * acknowledgement — or a reveal — on that callback has just lost it and must
   * recover rather than wait. Callers writing ordinary output can ignore the
   * answer; the renderer asks for its own seed on the overflow case.
   */
  write(bytes: OwnedTerminalBytes, onRendered?: () => void, generation?: number): boolean;
  /** Measures the CSS box in cells. Does not resize the terminal. */
  measure(): TerminalSize | undefined;
  /**
   * What this terminal turns pixels into: one cell's size and the chrome a
   * terminal spends out of the box it is given. Values, not a callback — the
   * client-size computation is arithmetic over them, and a stale value is
   * visible where an unanswerable callback was not.
   */
  measurements(): TerminalMeasurements | undefined;
  /** Reports xterm cell-metric changes, including changes caused only by DPR. */
  onMeasurementsChange(listener: () => void): () => void;
  /** Forces the grid tmux says this pane has, whatever the CSS box measured. */
  setGrid(size: TerminalSize): GridOutcome;
  focus(): void;
  blur(): void;
  onInput(listener: (input: TerminalInput) => void): () => void;
  onViewportChange(listener: (state: TerminalViewportState) => void): () => void;
  getSelection(): string;
  hasSelection(): boolean;
  onSelectionChange(listener: () => void): () => void;
  isAlternateScreenActive(): boolean;
  paste(text: string): void;
  search(query: string, direction?: "next" | "previous"): boolean;
  clearSearch(): void;
  scrollToBottom(): void;
  serialize(): string;
  drainAndSerialize(): Promise<DrainedTerminalSnapshot>;
  disposeGpuRenderer(): void;
  dispose(): void;
}

/**
 * No scroll easing at all.
 *
 * xterm quantises viewport scrolling to whole rows, so an animation duration
 * buys no intermediate positions to animate *through* — it only defers the
 * single row-aligned jump the wheel tick already decided on. At the 80ms this
 * used to hold, every wheel tick carried ~80ms of trailing ease that the user
 * reads as the terminal lagging their scroll: pure perceived latency for zero
 * smoothness. Zero means each tick lands on the frame it arrives in.
 */
const SMOOTH_SCROLL_DURATION_MS = 0;

/**
 * Closes a background bleed in xterm's serialize addon before it can paint.
 *
 * The addon ends the normal buffer by replaying the terminal's *live* pen and
 * then switches to the alternate screen with that pen still active. xterm
 * activates the alternate buffer with background-color-erase, so a pen caught
 * mid-frame holding an explicit background pre-fills the whole alternate
 * screen with it — and the replay skips blank runs rather than erasing them,
 * so the pre-fill survives in every cell the serializer chose not to write.
 * On restore that reads as two interleaved dark backgrounds split along
 * "written vs skipped" cells: banding that hugs the text and skips lines,
 * appearing exactly after the stall-recovery reseeds where restores happen.
 *
 * Resetting the pen immediately before the switch makes the pre-fill use the
 * default background, which is indistinguishable from the screen around it.
 * Applied at restore time rather than serialize time so states already sitting
 * in the cache are cleaned too.
 */
export function sanitizeSerializedScreen(serialized: string): string {
  return serialized.replaceAll("\u001b[?1049h", "\u001b[0m\u001b[?1049h");
}

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

export class XtermRenderer implements TerminalRenderer {
  readonly #terminal: Terminal;
  readonly #fit = new FitAddon();
  readonly #serialize = new SerializeAddon();
  readonly #search = new SearchAddon({ highlightLimit: 1_000 });
  readonly #viewportListeners = new Set<(state: TerminalViewportState) => void>();
  readonly #disposables: IDisposable[] = [];
  /**
   * Everything that only makes sense while the GPU renderer is mounted: the
   * atlas-change repaint and the staleness probe both read the addon, and a
   * pane that has dropped back to the DOM renderer has neither an atlas nor a
   * model for them to speak about.
   */
  readonly #webglDisposables: IDisposable[] = [];
  readonly #scheduler: TerminalWriteScheduler;
  readonly #generations = new TerminalGenerationWatermark();
  readonly #options: TerminalRendererOptions;
  #webgl?: WebglAddon;
  #newOutput = false;
  #lastViewport?: TerminalViewportState;
  #seedRequested = false;
  #drainPromise?: Promise<DrainedTerminalSnapshot>;
  #drainAbandoned = false;
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
      ...GHOSTTY_TEXT_OPTIONS,
      fontFamily: font.fontFamily,
      fontSize: font.fontSize,
      ignoreBracketedPasteMode: false,
      macOptionClickForcesSelection: true,
      rightClickSelectsWord: true,
      // Off by default (P12-U002/U003). xterm's screen-reader mode allocates a
      // string and dispatches an emitter event for every printed codepoint and
      // rewrites a DOM mirror of every row on every render, which an agent TUI
      // repainting at 1 Hz pays thousands of times a second; its mirror also
      // sits over the WebGL canvas with an un-overridden `::selection`
      // background, which is what painted highlight rectangles at stale
      // positions. The pane's own AX label and role, keyboard operability and
      // xterm's input textarea are unaffected either way. Phase 11 added the
      // settings surface that Phase 12 deferred this to, so a user who needs
      // terminal content read aloud can now turn it on.
      screenReaderMode: terminalScreenReaderMode(),
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
    this.#disposables.push(installOsc52ClipboardWrite(
      this.#terminal.parser,
      this.#options.onClipboardWrite ?? (() => undefined),
      this.#options.onClipboardWriteError,
    ));
    this.#scheduler = new TerminalWriteScheduler(
      (chunk, done) => this.#terminal.write(chunk, done),
      (callback) => window.requestAnimationFrame(callback),
      (handle) => window.cancelAnimationFrame(handle),
      256 * 1024,
      8 * 1024 * 1024,
      (pending) => {
        if (pending > 4 * 1024 * 1024) this.#options.onDiagnostic?.("Terminal output is catching up…");
        // The `#webgl` guard keeps the fallback banner up deliberately. A pane
        // that lost its GPU renderer never gets it back — nothing here remounts
        // the addon — so it renders through the DOM for the rest of its life,
        // and clearing the banner once the queue drains would tell the user the
        // degradation had passed when it had not. The catching-up message is
        // transient and its own condition still clears it; only the permanent
        // one persists.
        else if (pending === 0 && this.#webgl) this.#options.onDiagnostic?.(undefined);
      },
      (pending, records) => this.#requestSeed(
        `Terminal renderer queue exceeded its bound (${pending} bytes${records === undefined ? "" : `, ${records} records`}); requesting a fresh seed.`,
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
    this.#applyDeviceSafeLineHeight();
    const core = (this.#terminal as MeasurableTerminal)._core;
    const charSize = core?._charSizeService;
    const charSizeSubscription = charSize?.onCharSizeChange?.(() => this.#applyDeviceSafeLineHeight());
    if (charSizeSubscription) this.#disposables.push(charSizeSubscription);
    const dprSubscription = core?._coreBrowserService?.onDprChange?.(() => {
      // Let xterm finish updating its own DPR-dependent character metric first.
      queueMicrotask(() => this.#applyDeviceSafeLineHeight());
    });
    if (dprSubscription) this.#disposables.push(dprSubscription);
    this.#mountWebgl();
    this.#discardFallbackAtlas();
  }

  seed(bytes: OwnedTerminalBytes, onRendered?: () => void, generation = 0): void {
    this.#newOutput = false;
    // A seed is the whole screen, so it also re-bases the applied-generation
    // watermark. Without that reset, a seed from a new terminal epoch — whose
    // generations restart at 1 — would sit below a watermark the previous epoch
    // left behind, and every later restore and hide checkpoint would be
    // measured against a number from a stream that no longer exists.
    this.#generations.resetAuthoritativeStream();
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
    const decision = restoreDecision(throughGeneration, this.#generations.enqueuedGeneration, this.#scheduler.overflowed);
    if (decision.kind === "reseed") {
      this.#requestSeed(decision.reason);
      return false;
    }
    this.#newOutput = false;
    this.#scheduler.replace(
      new TextEncoder().encode(sanitizeSerializedScreen(serialized)),
      false,
      this.#enqueued(generation, onRendered),
    );
    this.#emitViewport();
    return true;
  }

  write(bytes: OwnedTerminalBytes, onRendered?: () => void, generation = 0): boolean {
    if (!this.#atBottom()) this.#newOutput = true;
    const queued = this.#scheduler.enqueueOwned(bytes, this.#enqueued(generation, onRendered));
    if (!queued && this.#scheduler.overflowed) {
      // Only an overflow refusal means bytes were lost. The scheduler also
      // refuses when it is disposed or sealed for the hide drain, and asking
      // for a seed then would be recovery for a pane that is going away.
      this.#requestSeed("Terminal output could not be queued for this pane; requesting a fresh seed.");
    }
    this.#emitViewport();
    return queued;
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
   * xterm observes device-pixel-ratio changes itself and recomputes its render
   * dimensions. The host's CSS box does not necessarily change at the same
   * time, so a ResizeObserver alone cannot tell the pane to remeasure.
   */
  onMeasurementsChange(listener: () => void): () => void {
    const core = (this.#terminal as MeasurableTerminal)._core;
    let disposed = false;
    let pending = false;
    const notify = () => {
      if (pending) return;
      pending = true;
      queueMicrotask(() => {
        pending = false;
        if (!disposed) listener();
      });
    };
    const dimensions = core?._renderService?.onDimensionsChange?.(notify);
    // RenderService updates for DPR internally, but xterm 6 does not emit its
    // dimensions event on that path. Its browser service does, so listen to
    // both and coalesce them after xterm's synchronous update has completed.
    const dpr = core?._coreBrowserService?.onDprChange?.(notify);
    return () => {
      disposed = true;
      dimensions?.dispose();
      dpr?.dispose();
    };
  }

  #applyDeviceSafeLineHeight(): void {
    if (this.#disposed) return;
    const measured = (this.#terminal as MeasurableTerminal)._core?._charSizeService?.height;
    const lineHeight = deviceSafeLineHeight(measured, window.devicePixelRatio);
    if (lineHeight !== undefined && this.#terminal.options.lineHeight !== lineHeight) {
      this.#terminal.options.lineHeight = lineHeight;
    }
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
    const options = { decorations: searchDecorations() };
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

  /**
   * Serializes this terminal once xterm has finished with everything it was
   * given — or, if xterm never answers, once the bound expires.
   *
   * A timed-out drain still returns a usable snapshot: `serialize` reads the
   * buffer as it stands, and the generation reported is the *applied* one, so
   * the checkpoint built from it claims only what xterm actually rendered and
   * the rest stays the host's to resend. What it must not do is let a
   * completion that arrives afterwards move that watermark: the checkpoint has
   * already been published by then, and a late advance would tell the next
   * reveal that output nobody displayed had been displayed. `#drainAbandoned`
   * freezes the watermark at exactly the number this snapshot reported.
   */
  drainAndSerialize(): Promise<DrainedTerminalSnapshot> {
    this.#drainPromise ??= settleWithin(
      this.#scheduler.sealAndDrain().then(() => this.#snapshot()),
      this.#options.drainTimeoutMs ?? DEFAULT_DRAIN_TIMEOUT_MS,
      () => {
        this.#drainAbandoned = true;
        recordPerfCounter("terminal.renderer.drainTimeouts");
        return this.#snapshot();
      },
    );
    return this.#drainPromise;
  }

  #snapshot(): DrainedTerminalSnapshot {
    return { serialized: this.serialize(), outputGeneration: this.#generations.appliedGeneration };
  }

  disposeGpuRenderer(): void {
    const hadWebgl = this.#webgl !== undefined;
    this.#webgl?.dispose();
    this.#webgl = undefined;
    for (const disposable of this.#webglDisposables.splice(0)) disposable.dispose();
    if (hadWebgl && !this.#disposed) this.#terminal.options.letterSpacing = 0;
  }

  onSelectionChange(listener: () => void): () => void {
    const disposable = this.#terminal.onSelectionChange(listener);
    return () => disposable.dispose();
  }

  isAlternateScreenActive(): boolean {
    return this.#terminal.buffer.active.type === "alternate";
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
    const applied = this.#generations.enqueued(generation, onRendered);
    return () => {
      // The hide checkpoint has already been published from an abandoned drain.
      // These pixels were never shown to anyone, so they may not be reported as
      // rendered after the fact.
      if (this.#drainAbandoned) return;
      applied();
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
   * Throws away any glyphs this terminal rasterised before the bundled face
   * arrived.
   *
   * xterm rasterises each glyph **once**, into a texture atlas it never
   * revisits, and it positions that glyph from the ink it finds relative to a
   * baseline it drew at. A glyph drawn in the fallback face therefore keeps the
   * fallback's baseline for as long as the atlas lives — and the atlas outlives
   * the font load, because nothing in xterm re-rasterises on `loadingdone`. The
   * result is a single row of text in two typefaces at two baselines: measured
   * on the user's 2× capture, `d e n o p` sat 3.4 device pixels — 1.7 CSS px —
   * below `a c g m r s`, which is exactly "some letters a pixel-plus lower than
   * their neighbours".
   *
   * `main.tsx` now loads the face before the app mounts, so in practice this
   * finds it already there and does nothing. It stays because that wait is
   * bounded: a face that resolves after the bound would otherwise leave a
   * two-typeface atlas on screen until the pane is closed, and nothing about
   * that failure is recoverable by the user. The wait is the same one `main.tsx`
   * performs, shared, so it covers every style the atlas can hold rather than
   * only the regular face — a late *bold* face splits the atlas just as visibly.
   *
   * Only the glyphs are recovered here, not the metrics: xterm re-measures the
   * cell on a `fontFamily`/`fontSize` change and on nothing else, so a face that
   * lands this late still leaves the tmux grid derived from the fallback cell.
   * Forcing that re-measure means writing an option we do not own to a value we
   * do not want and back, which is a worse trade than the bounded wait.
   *
   * Every live pane clears when the faces land, and the atlas is shared between
   * panes that look alike, so the last clear is the one that counts and the ones
   * before it are redundant. They all run in the same microtask drain, before
   * anything is rasterised against them, so the cost is the calls themselves.
   */
  #discardFallbackAtlas(): void {
    // Nothing can arrive after this moment, so no glyph rasterised from here on
    // can disagree with one rasterised a moment ago. That is the case on every
    // pane once the faces have settled, which is nearly always.
    if (!terminalFacesPending()) return;
    void terminalFacesReady().then(() => {
      if (this.#disposed) return;
      this.#terminal.clearTextureAtlas();
    }).catch(() => {
      // A face that cannot load leaves the fallback glyphs in place, which is
      // the best available outcome and not something the user can act on.
    });
  }

  #mountWebgl(): void {
    // Before the addon exists, because activating it is what builds the first
    // glyph atlas, and an atlas built before the hook is in place keeps the
    // heavier glyphs for as long as it lives. Outside the `try`, because that
    // `catch` speaks for WebGL: a failure in here reported as "WebGL
    // unavailable" would name the wrong subsystem and skip the renderer too.
    installAtlasFontSmoothing();
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl.dispose();
        if (this.#webgl === webgl) {
          this.#webgl = undefined;
          this.#terminal.options.letterSpacing = 0;
        }
        for (const disposable of this.#webglDisposables.splice(0)) disposable.dispose();
        // Disposing the WebGL addon drops xterm back to the *DOM* renderer:
        // xterm 6 has no canvas renderer and no `@xterm/addon-canvas` is
        // installed, so naming one sent every such report looking for a
        // renderer that does not exist.
        this.#options.onDiagnostic?.("WebGL context lost; using the slow DOM renderer.");
        recordIncident("render.webglFallback", { paneId: this.#options.paneId, reason: "contextLoss" });
      });
      // The shared atlas has no way to reach the panes drawing from it, and it
      // rewrites their glyph coordinates from under them: merging four pages
      // into one moves every glyph already rasterised, and each pane baked the
      // old coordinates into its vertex buffer the last time it touched a cell.
      // The merge happens *during* a paint — inside the glyph lookup of the row
      // being drawn — so the frame on screen ends up half pre-merge and half
      // post-merge, and the rows drawn before it are scattered garbage. Marking
      // the model stale is not enough on its own: it only corrects the *next*
      // frame, and a pane whose output has stopped has no next frame, so the
      // garbage stays until something else happens to dirty those rows.
      //
      // This is the missing schedule. The addon already announces every atlas
      // page change to every pane sharing the atlas (`onAddTextureAtlasCanvas`,
      // forwarded from the atlas by each `WebglRenderer`), so one repaint per
      // pane per announcement closes the hole with no patch of our own. It also
      // fires for an ordinary new page, which invalidates nothing — that costs
      // a wasted viewport repaint on an event that happens once per ~1500 newly
      // rasterised glyphs, which is far cheaper than the alternative of leaving
      // a pane wrong until the user scrolls it.
      this.#webglDisposables.push(webgl.onAddTextureAtlasCanvas(() => this.#repaintAfterAtlasChange()));
      this.#terminal.loadAddon(webgl);
      this.#webgl = webgl;
      this.#terminal.options.letterSpacing = WEBGL_CELL_SPACING;
      this.#webglDisposables.push({ dispose: watchAtlasStaleness(this.#options.paneId, webgl) });
      this.#options.onDiagnostic?.(undefined);
    } catch (error) {
      this.#webgl = undefined;
      this.#options.onDiagnostic?.("WebGL unavailable; using the slow DOM renderer.");
      console.warn("WebGL terminal renderer unavailable; using the DOM renderer", error);
      recordIncident("render.webglFallback", { paneId: this.#options.paneId, reason: String(error) });
    }
  }

  /**
   * One full-viewport repaint for one atlas change. `refresh` only queues the
   * rows; the addon's own `beginFrame` decides on the next animation frame
   * whether the change actually invalidated this pane's model, and rebuilds it
   * exactly once if it did.
   */
  #repaintAfterAtlasChange(): void {
    if (this.#disposed) return;
    this.#terminal.refresh(0, this.#terminal.rows - 1);
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
    const bufferLine = this.#terminal.buffer.active.getLine(bufferLineNumber - 1);
    const line = bufferLine?.translateToString(true);
    if (!line || !bufferLine) return undefined;
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
    if (this.#options.onOpenFilePath) {
      for (const link of terminalFileLinks(line)) {
        const cells = terminalFileLinkCellRange(bufferLine, link.start, link.end);
        if (!cells) continue;
        // A URL provider has already claimed this range. File-path recognition
        // deliberately excludes schemes, but keep the ownership explicit if
        // that vocabulary changes later.
        if (links.some((existing) => existing.range.start.x - 1 === cells.start)) continue;
        links.push({
          text: link.text,
          range: {
            start: { x: cells.start + 1, y: bufferLineNumber },
            end: { x: cells.end + 1, y: bufferLineNumber },
          },
          activate: (event) => {
            if (isTerminalFileLinkActivation(event)) this.#options.onOpenFilePath?.(link.text);
          },
        });
      }
    }
    return links.length ? links : undefined;
  }
}
