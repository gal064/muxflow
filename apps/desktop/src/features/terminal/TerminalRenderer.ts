import { Terminal, type IDisposable, type ILink } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SerializeAddon } from "@xterm/addon-serialize";
import { SearchAddon } from "@xterm/addon-search";
import { WebglAddon } from "@xterm/addon-webgl";
import { terminalScreenReaderMode } from "./accessibilityPreference";
import { installAtlasFontSmoothing } from "./atlasFontSmoothing";
import { watchAtlasStaleness } from "./atlasStaleProbe";
import { GHOSTTY_TEXT_OPTIONS, searchDecorations, terminalFacesPending, terminalFacesReady, terminalFont, terminalTheme } from "./theme";
import {
  deviceSafeCellSpacing,
  deviceSafeLineHeight,
  terminalMeasurements,
  type MeasurableTerminal,
  type TerminalMeasurements,
  type TerminalSize,
} from "./cellMetrics";
import type { OwnedTerminalBytes } from "./TerminalBytes";
import { composeHistoryPage, splitHistoryRows } from "./historyPage";
import { TerminalGenerationWatermark } from "./TerminalGenerationWatermark";
import { TerminalWriteScheduler } from "./TerminalWriteScheduler";
import { settleWithin } from "./timeBound";
import { recordPerfCounter } from "../../perf/probe";
import { recordIncident } from "../../diagnostics/incidents";
import { isTerminalFileLinkActivation } from "./terminalFilePaths";
import type { Platform } from "../../commands/registry";
import { installOsc52ClipboardWrite } from "./osc52Clipboard";
import { captureTerminalSelection, type TerminalSelectionSnapshot } from "./terminalSelection";
import { terminalLinksForBufferLine } from "./terminalLinks";
import {
  captureTerminalViewport,
  resizeTerminalPreservingViewport,
  restoreTerminalViewport,
  type TerminalViewportAnchor,
} from "./terminalViewport";

// Re-exported so the renderer stays the one import site for a pane's metrics.
export type { PixelBox, TerminalBoxChrome, TerminalMeasurements, TerminalSize } from "./cellMetrics";
export type { TerminalViewportAnchor } from "./terminalViewport";
export { cellsForBox, deviceSafeCellSpacing, deviceSafeLineHeight, terminalMeasurements } from "./cellMetrics";

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
  viewport: TerminalViewportAnchor;
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
   * Which modifier opens a file link, via `isTerminalFileLinkActivation`. The
   * renderer has no other reason to know the platform, and the default is the
   * same one the pane's own prop takes.
   */
  platform?: Platform;
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
  /** Initial text size; all other Ghostty-oriented text options stay fixed. */
  fontSize?: number;
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
  /**
   * Puts scrollback above what this terminal is showing, keeping the user's
   * viewport on the rows they were reading.
   *
   * xterm has no prepend, so this is a re-seed in disguise: the history and a
   * serialization of the current buffer are written together as one atomic
   * replace. That makes it refusable rather than partial — `"superseded"` means
   * nothing was touched and the caller may ask again.
   *
   * Output printed while the page was on the wire is not a reason to refuse.
   * Whatever arrived since is *inside* the serialization, and the anchor's
   * `skip` — the rows this buffer held when the page was asked for — is what
   * says how many rows of the page's tail those rows have already covered,
   * because tmux measures its capture from the display at the moment it runs
   * rather than at the moment it was asked. A scrollback that filled up in the
   * meantime is unrepairable, and so is a grid that changed under the page: a
   * row is only the same thing on both sides at one width.
   */
  prependHistory(history: OwnedTerminalBytes, anchor: HistoryPageAnchor): Promise<HistorySpliceOutcome>;
  /**
   * How many rows of scrollback sit above this terminal's screen.
   *
   * The number the host needs to answer a history request without repeating
   * itself: tmux measures its capture from the current display, so everything
   * that scrolled off since the seed is above it and already here.
   */
  readonly scrollbackRows: number;
  /**
   * The most rows of scrollback this terminal will ever hold above its screen.
   *
   * The ceiling `scrollbackRows` walks up to, and the reason paging has to be
   * able to end on this side as well as on tmux's: a pane whose `history-limit`
   * is larger than this can never reach the top of it, because every row
   * spliced in past the ceiling pushes an older one out and the count stops
   * moving. A pager that only watches tmux's `history_size` would ask for the
   * same clamped rows forever.
   */
  readonly scrollbackLimit: number;
  /**
   * Fires when the user asks to see above the top of what this pane holds:
   * either scrolling up onto row 0, or scrolling up again once already there.
   * Silent on the alternate screen, which has no scrollback.
   */
  onScrollbackTopReached(listener: () => void): () => void;
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
  /** Updates text metrics without replacing the terminal or its buffer. */
  setFontSize(fontSize: number): void;
  /** Forces the grid tmux says this pane has, whatever the CSS box measured. */
  setGrid(size: TerminalSize): GridOutcome;
  /** Restores a cached viewport when its serialized buffer used the same grid. */
  restoreViewport(anchor: TerminalViewportAnchor): void;
  /**
   * The cols and rows this terminal is rendering at.
   *
   * What a page of scrollback is anchored to: tmux's captured rows are this
   * terminal's rows only while the two agree on how wide a row is.
   */
  readonly grid: TerminalSize;
  /** The visible rows as text and the cursor, as a test reads the screen. */
  screenText(): { rows: string[]; cursor: [number, number] };
  /**
   * Fires when a `setGrid` actually reflowed the buffer.
   *
   * A reflow moves rows across the boundary between what tmux keeps in its
   * history and what it shows on its display, so anything asked for against
   * where that boundary used to be describes a buffer that no longer exists.
   */
  onGridApplied(listener: () => void): () => void;
  focus(): void;
  blur(): void;
  onInput(listener: (input: TerminalInput) => void): () => void;
  onViewportChange(listener: (state: TerminalViewportState) => void): () => void;
  getSelection(): string;
  hasSelection(): boolean;
  /** Snapshot text and immutable buffer evidence before asynchronous clipboard I/O. */
  getSelectionSnapshot?(): TerminalSelectionSnapshot;
  onSelectionChange(listener: () => void): () => void;
  isAlternateScreenActive(): boolean;
  /** DECCKM: whether cursor keys must be sent as SS3 rather than CSI. */
  isApplicationCursorMode(): boolean;
  paste(text: string): void;
  search(query: string, direction?: "next" | "previous"): boolean;
  clearSearch(): void;
  scrollToBottom(): void;
  /** Shows that live bytes exist without applying them to a historical view. */
  noteUnrenderedOutput(): void;
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
 * How much scrollback one pane keeps above its screen.
 *
 * One number for the three places that have to agree: xterm's own buffer bound,
 * the serialization a hide keeps, and the ceiling [`scrollbackLimit`] reports
 * to the pager. They were three literals, and the pager's termination now
 * depends on the third being the same as the first — a pane cannot page above
 * rows xterm has already dropped off the top of its buffer.
 */
const TERMINAL_SCROLLBACK_ROWS = 10_000;

/**
 * What sits between spliced history and the screen below it.
 *
 * The reset is not cosmetic: `capture-pane -e` ends on whatever attributes the
 * last history row left active, and without clearing them the screen below
 * would inherit that pen.
 */
const HISTORY_SEPARATOR = new TextEncoder().encode("\u001b[m\r\n");

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
 * What a page of scrollback was asked against.
 *
 * A page is only splicable onto the buffer it was asked for: `skip` says where
 * that buffer began, and the grid says what a row was. Both are read at the
 * moment of the request and quoted back at the splice.
 */
export interface HistoryPageAnchor {
  /** Rows of scrollback this terminal held when the page was asked for. */
  skip: number;
  columns: number;
  rows: number;
}

/**
 * What became of a page of scrollback.
 *
 * `overlapExceedsPage` is its own answer rather than one more refusal, because
 * it is the only one the caller can do something about: nothing was spliced,
 * but the reason is that the pane printed a whole page's worth while this page
 * crossed the link, and asking for the same size again asks the same question.
 */
export type HistorySpliceOutcome = "applied" | "superseded" | "overlapExceedsPage";

/** Why a page of scrollback was not put above a screen. Journalled, never spoken. */
type HistorySupersededReason =
  | "disposed"
  | "alternateScreen"
  | "overflowed"
  | "scrollbackCapped"
  | "overlapExceedsPage"
  /** The terminal reflowed between the request and the splice. */
  | "gridChanged"
  | "replaceRefused"
  /** The scheduler would not even take the barrier: disposed, sealed or overflowed. */
  | "barrierRefused";

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
): { kind: "apply" } | { kind: "reseed"; reason: string; incident: "pane.staleRestore" | "pane.overflowRestore" } {
  if (throughGeneration < lastEnqueuedGeneration) {
    return {
      kind: "reseed",
      reason: `A restore through generation ${throughGeneration} arrived for a pane that has already been given generation ${lastEnqueuedGeneration}; requesting a fresh seed.`,
      incident: "pane.staleRestore",
    };
  }
  if (overflowed) {
    return {
      kind: "reseed",
      reason: "The pane overflowed its renderer queue; a cached restore cannot replace the seed it needs.",
      incident: "pane.overflowRestore",
    };
  }
  return { kind: "apply" };
}

/**
 * The URL a click on a terminal link should open, or `undefined` when it should
 * open nothing: the click needs the same modifier that opens a file path — a
 * plain click stays inert so it selects text like any other cell — and only
 * `http(s)` links leave the app.
 */
export function activatedTerminalUrl(
  event: Pick<MouseEvent, "ctrlKey" | "metaKey">,
  platform: Platform,
  value: string,
): string | undefined {
  if (!isTerminalFileLinkActivation(event, platform)) return undefined;
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    return url.href;
  } catch {
    return undefined;
  }
}

export class XtermRenderer implements TerminalRenderer {
  readonly #terminal: Terminal;
  readonly #fit = new FitAddon();
  readonly #serialize = new SerializeAddon();
  readonly #search = new SearchAddon({ highlightLimit: 1_000 });
  readonly #viewportListeners = new Set<(state: TerminalViewportState) => void>();
  readonly #topListeners = new Set<() => void>();
  readonly #gridListeners = new Set<() => void>();
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
  #lastViewportY = 0;
  #lastViewport?: TerminalViewportState;
  #programmaticViewportMutation = false;
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
      fontSize: options.fontSize ?? font.fontSize,
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
      scrollback: TERMINAL_SCROLLBACK_ROWS,
      scrollOnUserInput: true,
      smoothScrollDuration: SMOOTH_SCROLL_DURATION_MS,
      windowOptions: {
        getCellSizePixels: true,
        getWinSizeChars: true,
        getWinSizePixels: true,
      },
      linkHandler: {
        activate: (event, url) => this.#activateLink(event, url),
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
    this.#disposables.push(this.#terminal.onScroll((viewportY) => {
      // xterm can emit several scroll positions while resize reflows wrapped
      // rows. None is the user's position until the marker has been restored,
      // and treating an intermediate zero as user intent starts history paging.
      if (this.#programmaticViewportMutation) return;
      if (this.#atBottom()) this.#newOutput = false;
      // The transition, not the state: a pane seeded with one screen sits at
      // row 0 from the moment it opens, and treating that as a request would
      // fetch scrollback nobody asked for on every reveal.
      if (viewportY === 0 && this.#lastViewportY > 0) this.#noteTopReached();
      this.#lastViewportY = viewportY;
      this.#emitViewport();
    }));
    this.#disposables.push(this.#terminal.registerLinkProvider({
      provideLinks: (line, callback) => callback(this.#linksForLine(line)),
    }));
  }

  open(element: HTMLElement): void {
    this.#terminal.open(element);
    // The other half of "the user asked for more". A pane holding exactly one
    // screen cannot scroll, so xterm emits no scroll event and the transition
    // above never happens — but pushing the wheel up against a top that will
    // not move is the same request, and for a screen-only seed it is the
    // ordinary one.
    const wheel = (event: WheelEvent) => {
      if (event.deltaY < 0 && this.#terminal.buffer.active.viewportY === 0) this.#noteTopReached();
    };
    element.addEventListener("wheel", wheel, { passive: true });
    this.#disposables.push({ dispose: () => element.removeEventListener("wheel", wheel) });
    this.#applyDeviceSafeCell();
    const core = (this.#terminal as MeasurableTerminal)._core;
    const charSize = core?._charSizeService;
    const charSizeSubscription = charSize?.onCharSizeChange?.(() => this.#applyDeviceSafeCell());
    if (charSizeSubscription) this.#disposables.push(charSizeSubscription);
    const dprSubscription = core?._coreBrowserService?.onDprChange?.(() => {
      // Let xterm finish updating its own DPR-dependent character metric first.
      queueMicrotask(() => this.#applyDeviceSafeCell());
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
    this.#notePositionReset();
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
      // Refusing a cached screen that the stream has already overtaken is the
      // recovery working, not a fault: every reconnect that restores a pane
      // from cache can produce one, and it was reaching the user as a notice
      // reading like an internal error. The journal keeps the whole fact; the
      // user keeps a pane that repaints from the host.
      recordIncident(
        decision.incident,
        decision.incident === "pane.staleRestore"
          ? {
            paneId: this.#options.paneId,
            throughGeneration,
            lastEnqueuedGeneration: this.#generations.enqueuedGeneration,
          }
          : { paneId: this.#options.paneId },
      );
      this.#requestSeed(decision.reason, false);
      return false;
    }
    this.#newOutput = false;
    this.#scheduler.replace(
      new TextEncoder().encode(sanitizeSerializedScreen(serialized)),
      false,
      this.#enqueued(generation, onRendered),
    );
    this.#notePositionReset();
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
   * Answers when the splice has happened, not when it was attempted.
   *
   * The rewrite waits on a barrier — xterm has to finish with everything it was
   * already given before its buffer can be serialized — and it can still be
   * refused there, by a disposal or by a scrollback that filled up. The caller
   * latches "this pane is holding its scrollback" on this answer, and latching
   * it on the attempt is how a pane stops asking for history it never received.
   *
   * A pane printing continuously is the ordinary case, not a refusal. The
   * anchor's `skip` is the row count the request quoted — where this buffer
   * began when it was asked — and tmux measures its capture from the display at
   * the moment it *runs*, so a page can come back overlapping the rows printed
   * in between. Everything on both sides of that arithmetic is a physical row
   * (the history capture drops `-J` so that it can be), which makes the overlap
   * countable here rather than guessable, and it is trimmed off the end of the
   * page before the two halves are composed.
   */
  prependHistory(history: OwnedTerminalBytes, anchor: HistoryPageAnchor): Promise<HistorySpliceOutcome> {
    // A TUI's alternate screen has no scrollback to prepend to, and rewriting
    // the buffer under it would destroy the frame the program is drawing.
    if (this.#disposed) return this.#supersede("disposed");
    if (this.isAlternateScreenActive()) return this.#supersede("alternateScreen");
    // The pane owes the host a scoped seed, and a splice is not that seed.
    if (this.#scheduler.overflowed) return this.#supersede("overflowed");
    return new Promise((resolve) => {
      // A zero-byte barrier, so the serialization below reads a buffer xterm
      // has finished with rather than one with bytes still inside its async
      // parser — those bytes would be serialized as absent, and the writes
      // behind the barrier are only replayable because they have *not* been
      // applied yet.
      const queued = this.#scheduler.enqueue(new Uint8Array(), () => {
        if (this.#disposed) return resolve(this.#noteHistorySuperseded("disposed"));
        if (this.isAlternateScreenActive()) return resolve(this.#noteHistorySuperseded("alternateScreen"));
        // The one thing continuous output can break beyond repair. Every row
        // printed since the request pushed one into this scrollback, and once it
        // is full xterm drops rows off the top to make room — so the captured
        // range no longer reaches what this buffer now starts with, and no
        // amount of trimming closes the hole that would leave in the middle of
        // the user's output.
        if (this.scrollbackRows >= this.scrollbackLimit) {
          return resolve(this.#noteHistorySuperseded("scrollbackCapped"));
        }
        // A row is the same thing on both sides of this only at one width. A
        // reflow moved rows across tmux's history boundary and rewrapped this
        // buffer, so the page's rows are neither this buffer's rows nor
        // countable against its skip. The pager orphans pages on a reflow it
        // hears about; this closes the window where one lands in between.
        if (this.#terminal.cols !== anchor.columns || this.#terminal.rows !== anchor.rows) {
          return resolve(this.#noteHistorySuperseded("gridChanged"));
        }
        // What the page and this buffer have in common. tmux measured its
        // capture from the display as it stood when the capture ran, and the
        // ordered stream has already put every row printed before that on this
        // terminal — so the rows this buffer has gained since the request are
        // exactly the rows at the foot of the page, and they come off it.
        const captured = splitHistoryRows(history);
        const overlap = Math.max(0, this.scrollbackRows - anchor.skip);
        // The whole page was rows this buffer already held. Nothing to splice,
        // and nothing to latch: the next reach-the-top asks from where this
        // buffer actually begins now, which is above everything this page held.
        // Answered in its own words, because the caller's remedy is to ask for
        // more rows next time rather than to ask again for the same.
        if (overlap >= captured.length) {
          this.#noteHistorySuperseded("overlapExceedsPage");
          return resolve("overlapExceedsPage");
        }
        const kept = overlap === 0 ? captured : captured.slice(0, captured.length - overlap);
        const page = composeHistoryPage(kept, anchor.columns);
        const screen = new TextEncoder().encode(this.#serializedScreenAtFullHeight());
        const spliced = new Uint8Array(page.byteLength + HISTORY_SEPARATOR.byteLength + screen.byteLength);
        spliced.set(page);
        spliced.set(HISTORY_SEPARATOR, page.byteLength);
        spliced.set(screen, page.byteLength + HISTORY_SEPARATOR.byteLength);
        this.#notePositionReset();
        const replaced = this.#scheduler.replace(
          spliced,
          false,
          () => {
            // Keep the user on the rows they were reading: the splice put
            // exactly `kept.length` rows above them, because a composed page
            // occupies as many rows as it was captured with whether xterm
            // wrapped them or this side broke them. Counted rather than measured
            // off the buffer — a length taken before and after is corrupted by
            // any output the scheduler coalesced into the same write.
            this.#terminal.scrollToLine(kept.length);
          },
          // Writes queued behind the barrier are not in the serialization and
          // have not been applied. They would have run after the barrier, so
          // running them after the splice is where they belong.
          true,
        );
        resolve(replaced ? "applied" : this.#noteHistorySuperseded("replaceRefused"));
      });
      if (!queued) resolve(this.#noteHistorySuperseded("barrierRefused"));
    });
  }

  /**
   * The screen serialized so that it re-renders at exactly its own height.
   *
   * The serialize addon trims the blank rows below the last content of a buffer
   * that holds no scrollback, and puts the cursor back with moves relative to
   * where its output ends. That is only right when nothing is written after it.
   * Behind a page of history the trimmed rows are missing: the page's last rows
   * slide into the top of the viewport, everything the screen showed sits that
   * many rows too low, and the cursor is moved to match — so a program that
   * addresses rows absolutely, a TUI repainting its footer, paints over the
   * wrong rows from then until its next full redraw. The trimmed rows go back
   * here, and the cursor is placed absolutely.
   */
  #serializedScreenAtFullHeight(): string {
    const serialized = sanitizeSerializedScreen(this.serialize());
    // With scrollback above it the addon emits every row, screen included.
    if (this.scrollbackRows > 0) return serialized;
    const missing = this.#terminal.rows - this.#serializedRows();
    if (missing <= 0) return serialized;
    const buffer = this.#terminal.buffer.active;
    // The addon ends with relative cursor moves, the live pen, and then the
    // modes it restores (`CSI ? n h`, `CSI n h`, `CSI ? 7 l`); the last two
    // are kept and re-emitted after the absolute cursor.
    const tail = /(\u001b\[\d+[AB])?(\u001b\[\d+[CD])?(\u001b\[[0-9;]*m)?((?:\u001b\[\??\d+[hl])*)$/;
    const [, , , pen = "", modes = ""] = serialized.match(tail) ?? [];
    const content = serialized.replace(tail, "");
    const cursor = `\u001b[${buffer.cursorY + 1};${buffer.cursorX + 1}H`;
    return `${content}\u001b[m${"\r\n".repeat(missing)}${cursor}${pen}${modes}`;
  }

  /**
   * How many rows the serialize addon emits for this scrollback-free screen:
   * through the last row holding a character, or a blank cell whose background
   * differs from the pen at that point — the addon's own rule for "content".
   */
  #serializedRows(): number {
    const buffer = this.#terminal.buffer.active;
    const pen = buffer.getNullCell();
    let last = 0;
    for (let y = 0; y < this.#terminal.rows; y += 1) {
      const line = buffer.getLine(y);
      if (!line) break;
      for (let x = 0; x < this.#terminal.cols; x += 1) {
        const cell = line.getCell(x);
        if (!cell) break;
        const sameBackground = cell.getBgColorMode() === pen.getBgColorMode()
          && cell.getBgColor() === pen.getBgColor();
        if (cell.getChars() === "" && sameBackground) continue;
        last = y;
        line.getCell(x, pen);
      }
    }
    return last + 1;
  }

  /** `#noteHistorySuperseded` for the paths that answer before the barrier. */
  #supersede(reason: HistorySupersededReason): Promise<HistorySpliceOutcome> {
    return Promise.resolve(this.#noteHistorySuperseded(reason));
  }

  /**
   * A page of scrollback that could not be put above this screen.
   *
   * Journalled, never spoken, and every path answers through here: a splice
   * that is refused silently is a pane that stops holding its scrollback with
   * nothing to say why. Nothing on screen is wrong — the pane is showing
   * everything it has, the rows are still in tmux — and the next time the user
   * reaches the top the question is asked again against the screen they are
   * actually looking at.
   */
  #noteHistorySuperseded(reason: HistorySupersededReason): "superseded" {
    recordIncident("pane.historySuperseded", {
      paneId: this.#options.paneId,
      reason,
      scrollbackRows: this.scrollbackRows,
      scrollbackLimit: this.scrollbackLimit,
    });
    return "superseded";
  }

  get grid(): TerminalSize {
    return { columns: this.#terminal.cols, rows: this.#terminal.rows };
  }

  screenText(): { rows: string[]; cursor: [number, number] } {
    const buffer = this.#terminal.buffer.active;
    const rows: string[] = [];
    for (let y = 0; y < this.#terminal.rows; y += 1) {
      rows.push(buffer.getLine(buffer.baseY + y)?.translateToString(true) ?? "");
    }
    return { rows, cursor: [buffer.cursorX, buffer.cursorY] };
  }

  get scrollbackRows(): number {
    // The normal buffer, never the active one: on the alternate screen
    // `buffer.active.length` is the frame the program is drawing, and none of
    // it is scrollback. `length` counts the screen's own rows too, so the rows
    // above it are what is left after the grid is taken off.
    return Math.max(0, this.#terminal.buffer.normal.length - this.#terminal.rows);
  }

  get scrollbackLimit(): number {
    // The option, not a re-derivation: xterm is the thing that enforces it, and
    // a second copy of the number here is a second thing to keep in step.
    return this.#terminal.options.scrollback ?? TERMINAL_SCROLLBACK_ROWS;
  }

  onScrollbackTopReached(listener: () => void): () => void {
    this.#topListeners.add(listener);
    return () => this.#topListeners.delete(listener);
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

  setFontSize(fontSize: number): void {
    if (this.#terminal.options.fontSize === fontSize) return;
    this.#terminal.options.fontSize = fontSize;
    this.#applyDeviceSafeCell();
  }

  /**
   * Re-derives both halves of the cell from the face xterm has just measured.
   *
   * They are applied together because they are the same fact in two axes and
   * because the horizontal half depends on which renderer owns the terminal:
   * WebGL floors the advance into device pixels and needs the loss back, the
   * DOM renderer keeps the fraction and must stay at zero. Deriving it in one
   * place is what keeps the column count from changing when a pane crosses that
   * boundary on a lost context.
   */
  #applyDeviceSafeCell(): void {
    if (this.#disposed) return;
    const charSize = (this.#terminal as MeasurableTerminal)._core?._charSizeService;
    const lineHeight = deviceSafeLineHeight(charSize?.height, window.devicePixelRatio);
    if (lineHeight !== undefined && this.#terminal.options.lineHeight !== lineHeight) {
      this.#terminal.options.lineHeight = lineHeight;
    }
    const letterSpacing = this.#webgl ? deviceSafeCellSpacing(charSize?.width, window.devicePixelRatio) : 0;
    if (this.#terminal.options.letterSpacing !== letterSpacing) {
      this.#terminal.options.letterSpacing = letterSpacing;
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
    this.#mutateViewport(() => resizeTerminalPreservingViewport(this.#terminal, { columns, rows }));
    for (const listener of this.#gridListeners) listener();
    return { kind: "applied", size: { columns, rows } };
  }

  restoreViewport(anchor: TerminalViewportAnchor): void {
    this.#mutateViewport(() => restoreTerminalViewport(this.#terminal, anchor));
  }

  onGridApplied(listener: () => void): () => void {
    this.#gridListeners.add(listener);
    return () => this.#gridListeners.delete(listener);
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

  getSelectionSnapshot(): TerminalSelectionSnapshot {
    return captureTerminalSelection(this.#terminal);
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

  noteUnrenderedOutput(): void {
    if (this.#disposed) return;
    this.#newOutput = true;
    this.#emitViewport();
  }

  serialize(): string {
    return this.#serialize.serialize({ scrollback: TERMINAL_SCROLLBACK_ROWS });
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
    return {
      serialized: this.serialize(),
      outputGeneration: this.#generations.appliedGeneration,
      viewport: captureTerminalViewport(this.#terminal),
    };
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

  isApplicationCursorMode(): boolean {
    return this.#terminal.modes.applicationCursorKeysMode;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#scheduler.dispose();
    this.disposeGpuRenderer();
    for (const disposable of this.#disposables) disposable.dispose();
    this.#terminal.dispose();
  }

  /**
   * Forgets where the viewport was, because the buffer under it is being
   * replaced.
   *
   * Without this, the scroll xterm reports as it resets to row 0 reads as the
   * user arriving at the top from wherever they had been — and a seed would
   * fetch the scrollback it just deliberately left behind, unasked.
   */
  #notePositionReset(): void {
    this.#lastViewportY = 0;
  }

  #noteTopReached(): void {
    if (this.#disposed || this.isAlternateScreenActive()) return;
    for (const listener of this.#topListeners) listener();
  }

  #atBottom(): boolean {
    const buffer = this.#terminal.buffer.active;
    return buffer.viewportY >= buffer.baseY;
  }

  /**
   * Makes one renderer-owned viewport mutation atomic to listeners.
   *
   * The xterm operations are synchronous, but resize and scroll both emit
   * synchronous scroll events. The guard hides their intermediate positions,
   * then publishes the one position the user can actually see.
   */
  #mutateViewport(mutate: () => void): void {
    const alreadyMutating = this.#programmaticViewportMutation;
    this.#programmaticViewportMutation = true;
    try {
      mutate();
    } finally {
      this.#programmaticViewportMutation = alreadyMutating;
      if (!alreadyMutating) {
        this.#lastViewportY = this.#terminal.buffer.active.viewportY;
        if (this.#atBottom()) this.#newOutput = false;
        this.#emitViewport();
      }
    }
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
   * The diagnostic is suppressed for exactly one class of caller: a cached
   * restore the live stream has already overtaken, which journals itself and
   * recovers silently because it is ordinary post-reconnect bookkeeping. Every
   * other refusal still speaks — a refused write is exactly the silence
   * P12-U003.7 was about. Only the *request* is deduped, so one overflow cannot
   * become a reseed storm; a request that fails reopens the latch here rather
   * than at the caller, and a seed landing clears it.
   */
  #requestSeed(reason: string, announce = true): void {
    if (announce) this.#options.onDiagnostic?.(reason);
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
      // The same derivation `setFontSize` and the DPR listener use, so a pane
      // renders the same columns whichever renderer is currently mounted.
      this.#applyDeviceSafeCell();
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

  #activateLink(event: Pick<MouseEvent, "ctrlKey" | "metaKey">, value: string): void {
    const url = activatedTerminalUrl(event, this.#options.platform ?? "linux", value);
    if (url) this.#options.onOpenLink?.(url);
  }

  #linksForLine(bufferLineNumber: number): ILink[] | undefined {
    const links: ILink[] = [];
    for (const link of terminalLinksForBufferLine(
      this.#terminal.buffer.active,
      this.#terminal.cols,
      bufferLineNumber,
    )) {
      if (link.kind === "web") {
        links.push({
          text: link.text,
          range: link.range,
          activate: (event) => this.#activateLink(event, link.text),
        });
        continue;
      }
      if (!this.#options.onOpenFilePath) continue;
      links.push({
        text: link.text,
        range: link.range,
        activate: (event) => {
          if (isTerminalFileLinkActivation(event, this.#options.platform ?? "linux")) {
            this.#options.onOpenFilePath?.(link.text);
          }
        },
      });
    }
    return links.length ? links : undefined;
  }
}
