// One mounted terminal (design doc §7.6, §9.5), without React: the attach
// lifecycle, seed/output delivery to the WebView page, the seed timers, resize
// debouncing, input, and the hide on unmount. The screen owns one of these per
// focus; tests drive it against a fake or the real host.

import { type HostConnection } from "../../protocol/HostConnection";
import {
  attachTerminal,
  resizeTerminal,
  requestTerminalHistory,
  requestTerminalSeed,
  selectTerminalSession,
  setTerminalVisibility,
  terminalInput,
} from "../../protocol/requests";
import type { SessionStore } from "../../store/sessionStore";
import type { FromPageMessage, ToPageMessage } from "./bridgeMessages";
import { toBase64, utf8Encode } from "./bytes";
import { CR } from "./chips";
import { sameGrid, windowGrid, type Grid } from "./sizing";
import type { TerminalRegistry } from "./terminalRegistry";

/** §9.5 states, as the screen renders them. */
export type TerminalPhase =
  /** Waiting for the page's first size and/or the connection. */
  | "preparing"
  /** select → resize → attach in flight, or attached and waiting for the seed (< 5 s: blank). */
  | "attaching"
  /** A seed has been handed to xterm. */
  | "seeded"
  /** No seed after attach + retry: "No output yet. The pane may be idle." */
  | "noOutput"
  /** TERMINAL_EXIT for this pane's session: "This pane has closed." */
  | "exited";

export interface TerminalSnapshot {
  phase: TerminalPhase;
  grid: Grid | undefined;
  /** The last request that failed, for the log and a toast. */
  lastError: string | undefined;
}

export interface TerminalPage {
  send(message: ToPageMessage): void;
}

/** Whether a person can be looking at the app; `appForeground.ts` reads it off `AppState`. */
export interface AppForeground {
  inForeground(): boolean;
  /** Calls `listener` each time the app comes to the foreground; returns the unsubscribe. */
  onForeground(listener: () => void): () => void;
}

/** A controller built without one is always in the foreground (tests, the live harness). */
const ALWAYS_FOREGROUND: AppForeground = { inForeground: () => true, onForeground: () => () => undefined };

/** Gap between Send's paste and the CR that submits it. */
export const SUBMIT_DELAY_MS = 100;

export interface TerminalControllerOptions {
  paneId: string;
  sessionId: string;
  store: SessionStore;
  registry: TerminalRegistry;
  getConnection: () => HostConnection | null;
  page: TerminalPage;
  onChange?: (snapshot: TerminalSnapshot) => void;
  log?: (line: string) => void;
  /** §7.6 step 3: 5 s to the seed retry, another 5 s to the hint. */
  seedTimeoutMs?: number;
  /** §7.6 step 6: 150 ms. */
  resizeDebounceMs?: number;
  foreground?: AppForeground;
}

export const SEED_TIMEOUT_MS = 5_000;
export const RESIZE_DEBOUNCE_MS = 150;
/** A failed select/resize/attach is retried after this, up to ATTACH_RETRY_LIMIT times. */
export const ATTACH_RETRY_MS = 2_000;
export const ATTACH_RETRY_LIMIT = 3;
/**
 * The least time between two sizes this controller sends (D6). The desktop
 * takes the window back on its user's keystrokes at the same rate, so two
 * people typing at once flip the window at most once every couple of seconds
 * rather than on every key.
 */
export const TAKE_INTERVAL_MS = 2_000;

// §7.6.1 scrollback paging: a screen-only seed carries nothing above the
// screen; reaching the top of the buffer fetches the history above it. The
// first page is small (a slow link carries it inside a frame), later pages
// double up to the ceiling — the desktop's numbers (`PaneHistoryPager.ts`).
export const HISTORY_PAGE_LINES = 300;
export const HISTORY_MAX_PAGE_LINES = 4_800;
/**
 * The host's MAX_HISTORY_SKIP_LINES, mirrored, and the page's xterm
 * `scrollback` — the same number on purpose: a skip at the host's clamp
 * would refetch the same rows forever, and a buffer that trims its top would
 * throw the reader to the bottom on every splice. Paging stops at either.
 */
export const HISTORY_MAX_SKIP_LINES = 10_000;
/** Splices replay everything since the seed; past this the rebuild is too heavy for a phone and paging stops. */
export const HISTORY_RETAINED_CAP_BYTES = 4 * 1024 * 1024;

export class TerminalController {
  readonly paneId: string;
  readonly sessionId: string;
  private phase: TerminalPhase = "preparing";
  private grid: Grid | undefined;
  private sentGrid: Grid | undefined;
  private lastError: string | undefined;
  private attached = false;
  private attaching = false;
  private reattachWanted = false;
  private stopped = false;
  private unregister: (() => void) | undefined;
  /** Last `TerminalBytes.generation` handed to xterm; the hide checkpoint and the stale-seed guard. */
  private lastGeneration = 0n;
  private seedRetryTimer: ReturnType<typeof setTimeout> | undefined;
  private seedHintTimer: ReturnType<typeof setTimeout> | undefined;
  private resizeTimer: ReturnType<typeof setTimeout> | undefined;
  private attachRetryTimer: ReturnType<typeof setTimeout> | undefined;
  private attachFailures = 0;
  /** When the last RESIZE_TERMINAL went out, for the take interval. */
  private lastResizeAt: number | undefined;
  /** An attach asked for while the app was in the background; runs on the return to the foreground. */
  private attachOnForeground = false;
  private unsubscribeForeground: (() => void) | undefined;
  // ---- §7.6.1 history paging state, reset by every seed ----
  /** Everything handed to xterm since the seed (the seed first); a splice replays it above nothing but history. */
  private retained: Uint8Array[] = [];
  private retainedBytes = 0;
  /** Rows of history already spliced in, across every page. */
  private splicedRows = 0;
  /** The pages spliced so far, newest (highest above the screen) first; every splice replays all of them. */
  private pages: Uint8Array[] = [];
  /** Serial of the request in flight, so a late rejection of an older one cannot unlatch a newer one. */
  private historySerial = 0;
  private historyLines = HISTORY_PAGE_LINES;
  private historyInFlight = false;
  /** The `skip` the in-flight request quoted: the rows the page held when it asked. */
  private historySkipInFlight = 0;
  /** Latched when the host said the top was reached, the skip hit the clamp, or retention overflowed. */
  private historyDone = false;
  private readonly seedTimeoutMs: number;
  private readonly resizeDebounceMs: number;
  private readonly foreground: AppForeground;

  constructor(private readonly options: TerminalControllerOptions) {
    this.paneId = options.paneId;
    this.sessionId = options.sessionId;
    this.seedTimeoutMs = options.seedTimeoutMs ?? SEED_TIMEOUT_MS;
    this.resizeDebounceMs = options.resizeDebounceMs ?? RESIZE_DEBOUNCE_MS;
    this.foreground = options.foreground ?? ALWAYS_FOREGROUND;
  }

  get snapshot(): TerminalSnapshot {
    return { phase: this.phase, grid: this.grid, lastError: this.lastError };
  }

  get generation(): bigint {
    return this.lastGeneration;
  }

  /** Registers for events and takes focus; the attach waits for the page's size (§7.6 step 1). */
  start(): void {
    if (this.stopped) throw new Error("controller already stopped");
    this.unregister = this.options.registry.register({
      paneId: this.paneId,
      sessionId: this.sessionId,
      seed: (bytes, generation) => this.seed(bytes, generation),
      output: (bytes, generation) => this.output(bytes, generation),
      history: (bytes, historySize, sizeKnown) => this.history(bytes, historySize, sizeKnown),
      exit: (detail) => this.exit(detail),
      onConnected: () => this.onConnected(),
    });
    this.options.store.getState().setFocusedPane(this.paneId);
    // Step 1 or a size withheld while the app was in the background (see
    // attach and resizeNow) runs when a person is looking again. Nothing more:
    // a window the laptop took meanwhile is taken back by the next input, not
    // by the unlock — a phone unlocked to read a message with this screen in
    // front is an app left open, the phone's window focus.
    this.unsubscribeForeground = this.foreground.onForeground(() => {
      if (this.stopped) return;
      if (this.attachOnForeground) {
        this.attachOnForeground = false;
        void this.attach();
      } else {
        this.scheduleResize();
      }
    });
    this.options.page.send({ t: "init" });
  }

  /** The WebView page reported something (§10.2). */
  onPageMessage(message: FromPageMessage): void {
    switch (message.t) {
      case "ready":
        return;
      case "size": {
        const grid = { cols: message.cols, rows: message.rows };
        if (sameGrid(grid, this.grid)) return;
        this.grid = grid;
        this.log(`size ${grid.cols}x${grid.rows}${message.cellWidth ? ` cell=${message.cellWidth.toFixed(2)}x${message.cellHeight?.toFixed(2)}` : ""}`);
        this.emit();
        if (this.attached || this.attaching) this.scheduleResize();
        else void this.attach();
        return;
      }
      case "written":
        return;
      case "atTop":
        this.onAtTop(message.above);
        return;
      case "log":
        this.log(`page: ${message.line}`);
    }
  }

  /** Every tap and every Send: one TERMINAL_INPUT, immediately (§7.6). */
  async sendInput(bytes: Uint8Array, options?: { paste?: boolean }): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) throw new Error("not connected");
    this.takeSizeIfLost();
    await connection.request(terminalInput(this.paneId, bytes, options));
  }

  /**
   * D6: typing here is using the phone, so the window follows the phone. tmux
   * sizes a window from whichever client took it last, and a keystroke on the
   * desktop takes it back; when the topology shows the window at another size
   * than this controller asked for, the next input here takes it again — at
   * most once per `TAKE_INTERVAL_MS`, and never a size the window already has.
   * The request leaves ahead of the input on the same connection; the host
   * writes them to different tmux clients, so tmux may apply either first,
   * and the redraw follows the size either way.
   */
  private takeSizeIfLost(): void {
    if (!this.attached || this.attaching || this.resizeTimer !== undefined || !this.grid) return;
    const actual = this.actualWindowGrid();
    if (!actual || sameGrid(actual, this.grid)) return;
    if (this.lastResizeAt !== undefined && Date.now() - this.lastResizeAt < TAKE_INTERVAL_MS) return;
    this.log(`take: window is ${actual.cols}x${actual.rows}`);
    void this.resizeNow(true);
  }

  /** The grid tmux has for this pane's window, from the topology snapshot. */
  private actualWindowGrid(): Grid | undefined {
    const state = this.options.store.getState();
    const windowId = state.panes[this.paneId]?.windowId;
    return windowId === undefined ? undefined : windowGrid(Object.values(state.panes), windowId);
  }

  /**
   * The input bar's Send (§9.5): the text as one paste, then a CR as a
   * keystroke. The two are separate requests because the host never merges a
   * paste with its neighbours, and the CR waits until the paste is acknowledged
   * plus `SUBMIT_DELAY_MS`, so a composer that treats an Enter inside a fast
   * burst as a newline sees the CR on its own and submits. A refused paste
   * sends no CR. Empty text is a bare CR with no delay.
   *
   * Deliberately not gated on `stopped`: a Back during the gap must still
   * deliver the CR, or the pasted text sits unsubmitted in the composer.
   */
  async submitText(text: string): Promise<void> {
    const body = utf8Encode(text);
    if (body.length > 0) {
      await this.sendInput(body, { paste: true });
      await new Promise<void>((resolve) => setTimeout(resolve, SUBMIT_DELAY_MS));
    }
    await this.sendInput(CR);
  }

  /**
   * §7.6 step 4: clear focus first, then hide with the connection epoch and
   * the last rendered generation. Resolves once the hide is answered (or was
   * impossible because the connection is gone).
   */
  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    this.clearSeedTimers();
    if (this.resizeTimer !== undefined) {
      clearTimeout(this.resizeTimer);
      this.resizeTimer = undefined;
    }
    if (this.attachRetryTimer !== undefined) {
      clearTimeout(this.attachRetryTimer);
      this.attachRetryTimer = undefined;
    }
    this.unregister?.();
    this.unregister = undefined;
    this.unsubscribeForeground?.();
    this.unsubscribeForeground = undefined;
    const store = this.options.store.getState();
    if (store.focusedPaneId === this.paneId) store.setFocusedPane(undefined);
    const connection = this.liveConnection();
    // An attach still in flight hides itself when it completes (see attach()).
    if (!connection || !this.attached) return;
    await this.hide(connection);
  }

  private async hide(connection: HostConnection): Promise<void> {
    this.attached = false;
    try {
      await connection.request(setTerminalVisibility(this.paneId, false, {
        terminalEpoch: connection.connectionEpoch,
        generationCutoff: this.lastGeneration,
      }));
      this.log("hide → ok");
    } catch (error) {
      this.log(`hide.failed ${describe(error)}`);
    }
  }

  // ---- events from the connection --------------------------------------------

  private seed(bytes: Uint8Array, generation: bigint): void {
    if (this.stopped) return;
    // §7.6 step 2: a seed older than output already written is stale.
    if (generation !== 0n && generation < this.lastGeneration) {
      this.log(`seed.stale generation=${generation} < ${this.lastGeneration} (${bytes.byteLength} bytes discarded)`);
      return;
    }
    this.clearSeedTimers();
    if (generation > this.lastGeneration) this.lastGeneration = generation;
    // A seed resets the page, so it resets the history ledger too: the pane's
    // scrollback above this screen is unfetched again.
    this.retained = [bytes];
    this.retainedBytes = bytes.byteLength;
    this.splicedRows = 0;
    this.pages = [];
    this.historyLines = HISTORY_PAGE_LINES;
    this.historyInFlight = false;
    this.historyDone = false;
    this.options.page.send({ t: "seed", b64: toBase64(bytes) });
    this.log(`seed ${bytes.byteLength} bytes generation=${generation}`);
    if (this.phase !== "exited") this.setPhase("seeded");
  }

  private output(bytes: Uint8Array, generation: bigint): void {
    if (this.stopped) return;
    if (generation > this.lastGeneration) this.lastGeneration = generation;
    // Credit for these bytes is acknowledged by HostConnection as soon as they
    // are handed to the page (§7.7 "handed to xterm"); the page's `written`
    // echo is diagnostic only.
    this.retain(bytes);
    this.options.page.send({ t: "out", b64: toBase64(bytes) });
    // Output without a seed (host-abnormal) still means the pane is alive.
    if (this.phase === "noOutput" || this.phase === "attaching") {
      this.clearSeedTimers();
      this.setPhase("seeded");
    }
  }

  // ---- §7.6.1 scrollback paging -----------------------------------------------

  private retain(bytes: Uint8Array): void {
    // Nothing will splice again: neither the tail nor the pages are needed.
    if (this.historyDone) {
      if (this.retainedBytes > 0) {
        this.retained = [];
        this.retainedBytes = 0;
        this.pages = [];
      }
      return;
    }
    this.retained.push(bytes);
    this.retainedBytes += bytes.byteLength;
    if (this.retainedBytes > HISTORY_RETAINED_CAP_BYTES) {
      // The splice replays this buffer wholesale; past the cap that rebuild is
      // heavier than the scrollback is worth on a phone.
      this.retained = [];
      this.retainedBytes = 0;
      this.historyDone = true;
      this.log("history.retention.dropped (cap exceeded)");
    }
  }

  /** The page hit the top of its buffer holding `above` scrollback rows. */
  private onAtTop(above: number): void {
    if (this.stopped || this.historyInFlight || this.historyDone || this.phase !== "seeded") return;
    const connection = this.liveConnection();
    if (!connection) return;
    // tmux measures from the current display, so everything the page holds
    // above the screen — rows that scrolled off since the seed and the pages
    // spliced so far alike — is the skip. `above` counts both.
    const skip = above;
    // The page's scrollback is the host's clamp: at it, nothing more can be
    // held, and a request past it would answer with the rows at the clamp
    // on every reach-the-top.
    if (skip >= HISTORY_MAX_SKIP_LINES) {
      this.historyDone = true;
      this.log(`history.done skip=${skip} at the scrollback limit`);
      return;
    }
    const lines = Math.min(this.historyLines, HISTORY_MAX_SKIP_LINES - skip);
    const serial = ++this.historySerial;
    this.historyInFlight = true;
    this.historySkipInFlight = skip;
    this.log(`history.request lines=${lines} skip=${skip}`);
    connection.request(requestTerminalHistory(this.paneId, lines, skip)).catch((error: unknown) => {
      if (this.historySerial === serial) this.historyInFlight = false;
      this.log(`history.request.failed ${describe(error)}`);
    });
  }

  /** One TERMINAL_HISTORY answer; compose the splice and hand it to the page. */
  private history(bytes: Uint8Array, historySize: number, sizeKnown: boolean): void {
    if (this.stopped) return;
    const wasInFlight = this.historyInFlight;
    this.historyInFlight = false;
    // An answer for a request this attach did not make (a reseed raced it) has
    // nothing to splice against.
    if (!wasInFlight || this.historyDone) {
      this.log(`history.discarded ${bytes.byteLength} bytes`);
      return;
    }
    let rows = bytes.byteLength === 0 ? 0 : countRows(bytes);
    // tmux answers a range that lies entirely above its history with one row
    // rather than with nothing, and clamps one that runs past the top. When
    // the probe answered, `#{history_size}` says how many rows exist above
    // the display, so only that many beyond the skip are real; the rest is the
    // clamp row, and splicing it would duplicate a screen row above the top.
    if (sizeKnown) {
      const available = Math.max(0, historySize - this.historySkipInFlight);
      if (rows > available) {
        this.log(`history.clamped rows=${rows} available=${available}`);
        rows = available;
        bytes = available === 0 ? new Uint8Array(0) : takeRows(bytes, available);
      }
    }
    if (rows > 0) {
      // Every page fetched so far goes back in, newest (topmost) first: the
      // splice rebuilds the whole buffer, so a page left out would be a hole.
      this.pages.unshift(bytes);
      const tail = concat(this.retained, this.retainedBytes);
      this.options.page.send({ t: "splice", hist: toBase64(joinRows(this.pages)), rowsAdded: rows, tail: toBase64(tail) });
      this.splicedRows += rows;
      this.historyLines = Math.min(this.historyLines * 2, HISTORY_MAX_PAGE_LINES);
    }
    // `#{history_size}` is the fact of the top: everything at or past it is the
    // end. An unanswered probe (pane gone mid-capture) means ask again, and
    // tmux answers a range entirely above its history with one row, so an
    // empty-ish answer is not the signal.
    if (sizeKnown && this.historySkipInFlight + rows >= historySize) this.historyDone = true;
    this.log(`history ${bytes.byteLength} bytes rows=${rows} spliced=${this.splicedRows} size=${sizeKnown ? historySize : "?"}${this.historyDone ? " done" : ""}`);
  }

  private exit(detail: string): void {
    this.log(`exit ${detail}`);
    this.clearSeedTimers();
    this.setPhase("exited");
  }

  /** §7.6 step 5: every reconnect is a new connection; run step 1 again. */
  private onConnected(): void {
    if (this.stopped) return;
    this.attached = false;
    this.attachFailures = 0;
    this.sentGrid = undefined;
    // The generation counter lives in the daemon; a restarted daemon starts it
    // over, and the first seed of a fresh attach is never stale.
    this.lastGeneration = 0n;
    if (this.phase === "exited") this.setPhase("preparing");
    void this.attach();
  }

  // ---- attach ----------------------------------------------------------------

  private async attach(): Promise<void> {
    if (this.stopped || !this.grid || this.attached) return;
    if (this.attaching) {
      this.reattachWanted = true;
      return;
    }
    // Only while a person can be looking (D6, §7.6 step 5). The whole of step
    // 1 waits, not just the resize: selecting the session takes the host's
    // control client out of `ignore-size`, and one that has never been sent a
    // size would size the windows from tmux's 80x24 default. A reconnect with
    // the screen left open in a pocket must not touch the laptop's windows.
    if (!this.foreground.inForeground()) {
      this.attachOnForeground = true;
      return;
    }
    // A pending retry would otherwise re-run select → resize → attach on top
    // of this one, and every extra ATTACH resets the page with a new seed.
    if (this.attachRetryTimer !== undefined) {
      clearTimeout(this.attachRetryTimer);
      this.attachRetryTimer = undefined;
    }
    const connection = this.liveConnection();
    if (!connection) return;
    this.attaching = true;
    this.attached = false;
    this.lastError = undefined;
    this.setPhase(this.phase === "exited" ? "exited" : "attaching");
    const grid = this.grid;
    try {
      // The order is load-bearing: the host sizes the *selected* session's
      // control client and refuses a resize before one exists (§7.6 step 1).
      this.options.store.getState().setFocusedPane(this.paneId);
      await connection.request(selectTerminalSession(this.sessionId));
      if (this.stopped) return; // left before the reveal: nothing to hide
      this.lastResizeAt = Date.now();
      await connection.request(resizeTerminal(grid.cols, grid.rows));
      this.sentGrid = grid;
      if (this.stopped) return;
      await connection.request(attachTerminal(this.sessionId, this.paneId));
      this.attached = true;
      this.attachFailures = 0;
      // The attach mounts the pane; it does not photograph it. A session whose
      // control client already exists (selected earlier, or the window was
      // just created) commits the pane without a capture, and since the host's
      // hide/reveal rework flipping it visible captures nothing either — the
      // desktop always follows with an explicit reveal. This is the phone's:
      // idempotent, because a capture already in flight for a fresh
      // attachment coalesces it (`capture_in_flight`).
      if (!this.stopped) {
        connection.request(requestTerminalSeed(this.paneId)).catch((error: unknown) => this.log(`seed.request.failed ${describe(error)}`));
      }
      if (this.stopped) {
        // Backed out mid-attach: the attach still revealed the pane, so hide it
        // (§7.6 step 4) — unless a successor controller already owns the pane,
        // whose own select → resize → attach this late hide would undo.
        if (this.options.registry.get(this.paneId) === undefined) await this.hide(connection);
        else this.attached = false;
        return;
      }
      this.log(`attached ${grid.cols}x${grid.rows}`);
      if (this.phase !== "seeded") this.startSeedTimers();
    } catch (error) {
      this.lastError = describe(error);
      this.attachFailures += 1;
      this.log(`attach.failed (${this.attachFailures}) ${this.lastError}`);
      this.emit();
      if (!this.stopped && this.attachFailures < ATTACH_RETRY_LIMIT) {
        this.attachRetryTimer = setTimeout(() => {
          this.attachRetryTimer = undefined;
          if (!this.attached) void this.attach();
        }, ATTACH_RETRY_MS);
      }
    } finally {
      this.attaching = false;
    }
    if (this.reattachWanted) {
      this.reattachWanted = false;
      this.attached = false;
      void this.attach();
    } else if (this.attached && !sameGrid(this.sentGrid, this.grid)) {
      this.scheduleResize();
    }
  }

  private startSeedTimers(): void {
    this.clearSeedTimers();
    this.seedRetryTimer = setTimeout(() => {
      this.seedRetryTimer = undefined;
      const connection = this.liveConnection();
      if (!connection || this.stopped || this.phase === "seeded") return;
      this.log("seed.timeout → REQUEST_TERMINAL_SEED");
      connection.request(requestTerminalSeed(this.paneId)).catch((error: unknown) => this.log(`seed.request.failed ${describe(error)}`));
      this.seedHintTimer = setTimeout(() => {
        this.seedHintTimer = undefined;
        if (!this.stopped && this.phase === "attaching") this.setPhase("noOutput");
      }, this.seedTimeoutMs);
    }, this.seedTimeoutMs);
  }

  private clearSeedTimers(): void {
    if (this.seedRetryTimer !== undefined) clearTimeout(this.seedRetryTimer);
    if (this.seedHintTimer !== undefined) clearTimeout(this.seedHintTimer);
    this.seedRetryTimer = undefined;
    this.seedHintTimer = undefined;
  }

  /** §7.6 step 6: re-measure → resizeTerminal, debounced 150 ms. */
  private scheduleResize(): void {
    if (this.resizeTimer !== undefined) clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => {
      this.resizeTimer = undefined;
      void this.resizeNow();
    }, this.resizeDebounceMs);
  }

  /** `take`: the grid was sent before, but the window is not at it (D6). */
  private async resizeNow(take = false): Promise<void> {
    const connection = this.liveConnection();
    const grid = this.grid;
    if (!connection || !grid || this.stopped || !this.attached || this.attaching) return;
    if (!take && sameGrid(grid, this.sentGrid)) return;
    // Nobody is looking: the keyboard hiding as the app goes to the background
    // is not the phone being used. Sent on the return to the foreground.
    if (!this.foreground.inForeground()) return;
    this.sentGrid = grid;
    this.lastResizeAt = Date.now();
    try {
      await connection.request(resizeTerminal(grid.cols, grid.rows));
      this.log(`resize ${grid.cols}x${grid.rows} → ok`);
    } catch (error) {
      this.sentGrid = undefined;
      this.log(`resize.failed ${describe(error)}`);
    }
  }

  // ---- helpers ---------------------------------------------------------------

  private liveConnection(): HostConnection | null {
    const connection = this.options.getConnection();
    return connection && connection.state === "connected" ? connection : null;
  }

  private setPhase(phase: TerminalPhase): void {
    if (this.phase === phase) return;
    this.phase = phase;
    this.emit();
  }

  private emit(): void {
    this.options.onChange?.(this.snapshot);
  }

  private log(line: string): void {
    this.options.log?.(`[muxflow] terminal ${this.paneId} ${line}`);
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Rows in a history payload: CRLF separators + 1, and no trailing separator. */
function countRows(bytes: Uint8Array): number {
  let rows = 1;
  for (let index = 0; index + 1 < bytes.byteLength; index += 1) {
    if (bytes[index] === 0x0d && bytes[index + 1] === 0x0a) rows += 1;
  }
  return rows;
}

/** The last `count` rows of a CRLF-joined page: tmux fills a clamped range from the top, so the real rows are at the end. */
function takeRows(bytes: Uint8Array, count: number): Uint8Array {
  let seen = 0;
  for (let index = bytes.byteLength - 2; index >= 0; index -= 1) {
    if (bytes[index] === 0x0d && bytes[index + 1] === 0x0a) {
      seen += 1;
      if (seen === count) return bytes.subarray(index + 2);
    }
  }
  return bytes;
}

/** Pages joined with one CRLF between them (each page carries none at its ends). */
function joinRows(pages: readonly Uint8Array[]): Uint8Array {
  const separator = Uint8Array.of(0x0d, 0x0a);
  const pieces: Uint8Array[] = [];
  let total = 0;
  pages.forEach((page, index) => {
    if (index > 0) {
      pieces.push(separator);
      total += 2;
    }
    pieces.push(page);
    total += page.byteLength;
  });
  return concat(pieces, total);
}

function concat(pieces: readonly Uint8Array[], total: number): Uint8Array {
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const piece of pieces) {
    joined.set(piece, offset);
    offset += piece.byteLength;
  }
  return joined;
}
