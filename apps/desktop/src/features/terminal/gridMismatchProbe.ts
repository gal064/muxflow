import type { TerminalSize } from "./cellMetrics";

/**
 * A pane still rendering at a grid its own box stopped agreeing with.
 *
 * The sizing model is deliberately two-sided (see `reconcilePaneGrid` and
 * `refitPaneGridToBox`): tmux's grid is what the terminal renders at, while the
 * measured CSS box only decides what client size to ask tmux for. The two are
 * allowed to disagree for as long as tmux's answer is in flight, and the pane
 * shows the cost of that window — text clipped at the bottom while a shrink is
 * pending, a dead band while a growth is. Every path that opens the window also
 * closes it, so the disagreement is transient by construction.
 *
 * Unless the round trip is lost. Then the same two-sided state is permanent, and
 * it is the one failure in this file's neighbourhood that raises no error at
 * all: nothing throws, nothing reconnects, no watchdog counts anything, and the
 * pane simply renders short until a tab close and reopen remeasures it. This
 * watches for exactly that shape — a disagreement that outlives any answered
 * round trip — and writes one journal line when it finds one.
 *
 * Journal-only, and emphatically so: it never resizes, never asks tmux for
 * anything, and never touches what is on screen. A tripwire that healed the
 * state it observes would erase the evidence of how the state was reached.
 */

/**
 * How long a disagreement must hold before it is believed.
 *
 * A drag legitimately disagrees for the whole debounce plus a host round trip
 * plus a topology rediscovery; ten seconds is orders of magnitude past that, so
 * anything still standing here is not waiting on an answer that is coming.
 */
export const GRID_MISMATCH_SUSTAIN_MS = 10_000;

/**
 * How far apart the two grids must be to count as disagreeing.
 *
 * tmux spends a column on the divider between split panes and the layout hands
 * each pane a rounded percentage of its container, so a one-cell difference is
 * the normal resting state of a healthy pane and must never reach the journal.
 * The clipping this exists for is measured in many cells, not one.
 */
export const GRID_MISMATCH_MIN_DELTA = 2;

/** One line per episode per pane, not one per remeasurement. */
export const GRID_MISMATCH_INCIDENT_INTERVAL_MS = 60_000;

export interface GridMismatchIncident {
  kind: "pane.gridMismatch";
  paneId: string;
  tmuxColumns: number;
  tmuxRows: number;
  measuredColumns: number;
  measuredRows: number;
  /** How long the disagreement had held when it was recorded. */
  msSustained: number;
}

export interface GridMismatchProbeOptions {
  onIncident: (incident: GridMismatchIncident) => void;
  /** Injected so tests can drive the clock independently of the timers. */
  now?: () => number;
}

export interface GridMismatchProbe {
  /**
   * The grid a pane is rendering at, and what its box measured for it.
   *
   * Called wherever a grid is applied or reconciled. `measured` is undefined
   * when the box measured nothing usable, which is not evidence of a mismatch
   * and clears whatever was pending.
   */
  noteGrids(paneId: string, tmux: TerminalSize, measured: TerminalSize | undefined): void;
  /** Forgets a pane's pending disagreement — hidden, unmounted, superseded. */
  clear(paneId: string): void;
  dispose(): void;
}

interface PendingMismatch {
  /** When the episode started, on the injected clock. */
  armedAt: number;
  tmux: TerminalSize;
  measured: TerminalSize;
  timer: ReturnType<typeof setTimeout>;
}

function disagrees(tmux: TerminalSize, measured: TerminalSize): boolean {
  return Math.abs(tmux.columns - measured.columns) >= GRID_MISMATCH_MIN_DELTA
    || Math.abs(tmux.rows - measured.rows) >= GRID_MISMATCH_MIN_DELTA;
}

export function createGridMismatchProbe(
  { onIncident, now = () => Date.now() }: GridMismatchProbeOptions,
): GridMismatchProbe {
  const pending = new Map<string, PendingMismatch>();
  const lastIncidentAt = new Map<string, number>();

  const cancel = (paneId: string): void => {
    const open = pending.get(paneId);
    if (!open) return;
    pending.delete(paneId);
    clearTimeout(open.timer);
  };

  return {
    noteGrids(paneId, tmux, measured) {
      if (!measured || !disagrees(tmux, measured)) {
        cancel(paneId);
        return;
      }
      const open = pending.get(paneId);
      // One episode, however much the numbers move inside it. A pane being
      // dragged remeasures on every frame, and restarting the clock on each new
      // pair would mean a disagreement that never stops disagreeing also never
      // reaches its own deadline.
      if (open) {
        open.tmux = { columns: tmux.columns, rows: tmux.rows };
        open.measured = { columns: measured.columns, rows: measured.rows };
        return;
      }
      const armedAt = now();
      const timer = setTimeout(() => {
        const fired = pending.get(paneId);
        // Cleared out first: the rate limit below may swallow this line, and a
        // swallowed episode must still leave the pane free to arm a new one.
        pending.delete(paneId);
        if (!fired) return;
        const at = now();
        const previous = lastIncidentAt.get(paneId);
        if (previous !== undefined && at - previous < GRID_MISMATCH_INCIDENT_INTERVAL_MS) return;
        lastIncidentAt.set(paneId, at);
        onIncident({
          kind: "pane.gridMismatch",
          paneId,
          // The most recent pair, not the pair that armed the timer: what the
          // pane is showing now is what an investigation has to reproduce.
          tmuxColumns: fired.tmux.columns,
          tmuxRows: fired.tmux.rows,
          measuredColumns: fired.measured.columns,
          measuredRows: fired.measured.rows,
          msSustained: at - fired.armedAt,
        });
      }, GRID_MISMATCH_SUSTAIN_MS);
      pending.set(paneId, {
        armedAt,
        tmux: { columns: tmux.columns, rows: tmux.rows },
        measured: { columns: measured.columns, rows: measured.rows },
        timer,
      });
    },
    clear(paneId) {
      cancel(paneId);
    },
    dispose() {
      for (const open of pending.values()) clearTimeout(open.timer);
      pending.clear();
      lastIncidentAt.clear();
    },
  };
}
