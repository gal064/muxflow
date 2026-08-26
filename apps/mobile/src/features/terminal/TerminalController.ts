// One mounted terminal (design doc §7.6, §9.5), without React: the attach
// lifecycle, seed/output delivery to the WebView page, the seed timers, resize
// debouncing, input, and the hide on unmount. The screen owns one of these per
// focus; tests drive it against a fake or the real host.

import { type HostConnection } from "../../protocol/HostConnection";
import {
  attachTerminal,
  resizeTerminal,
  requestTerminalSeed,
  selectTerminalSession,
  setTerminalVisibility,
  terminalInput,
} from "../../protocol/requests";
import type { SessionStore } from "../../store/sessionStore";
import type { FromPageMessage, ToPageMessage } from "./bridgeMessages";
import { toBase64 } from "./bytes";
import { sameGrid, type Grid } from "./sizing";
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
}

export const SEED_TIMEOUT_MS = 5_000;
export const RESIZE_DEBOUNCE_MS = 150;
/** A failed select/resize/attach is retried after this, up to ATTACH_RETRY_LIMIT times. */
export const ATTACH_RETRY_MS = 2_000;
export const ATTACH_RETRY_LIMIT = 3;

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
  private readonly seedTimeoutMs: number;
  private readonly resizeDebounceMs: number;

  constructor(private readonly options: TerminalControllerOptions) {
    this.paneId = options.paneId;
    this.sessionId = options.sessionId;
    this.seedTimeoutMs = options.seedTimeoutMs ?? SEED_TIMEOUT_MS;
    this.resizeDebounceMs = options.resizeDebounceMs ?? RESIZE_DEBOUNCE_MS;
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
      exit: (detail) => this.exit(detail),
      onConnected: () => this.onConnected(),
    });
    this.options.store.getState().setFocusedPane(this.paneId);
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
      case "log":
        this.log(`page: ${message.line}`);
    }
  }

  /** Every tap and every Send: one TERMINAL_INPUT, immediately (§7.6). */
  async sendInput(bytes: Uint8Array): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) throw new Error("not connected");
    await connection.request(terminalInput(this.paneId, bytes));
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
    this.options.page.send({ t: "out", b64: toBase64(bytes) });
    // Output without a seed (host-abnormal) still means the pane is alive.
    if (this.phase === "noOutput" || this.phase === "attaching") {
      this.clearSeedTimers();
      this.setPhase("seeded");
    }
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
      await connection.request(resizeTerminal(grid.cols, grid.rows));
      this.sentGrid = grid;
      if (this.stopped) return;
      await connection.request(attachTerminal(this.sessionId, this.paneId));
      this.attached = true;
      this.attachFailures = 0;
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

  private async resizeNow(): Promise<void> {
    const connection = this.liveConnection();
    const grid = this.grid;
    if (!connection || !grid || this.stopped || !this.attached || this.attaching) return;
    if (sameGrid(grid, this.sentGrid)) return;
    this.sentGrid = grid;
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
