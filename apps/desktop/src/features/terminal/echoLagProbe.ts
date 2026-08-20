/**
 * How long a keystroke took to come back, journalled when it took too long.
 *
 * Typing lag is the one symptom the incident journal has never been able to
 * describe: nothing fails, nothing reconnects, and the pane simply echoes
 * seconds late. This measures the gap the user actually feels — input
 * dispatched for a pane until the next output delivered to that pane — and
 * records the outliers so a single journal line can separate a full delivery
 * window from a link that has gone quiet from an echo that arrived late anyway.
 *
 * Journal-only: nothing here touches input, delivery, or rendering.
 */

/** Above the point where an echo stops feeling instant on a remote link. */
export const ECHO_LAG_THRESHOLD_MS = 400;

/**
 * When a pending measurement is abandoned.
 *
 * A keystroke that has not echoed in five seconds is the lag the user is
 * complaining about, and holding the measurement open longer only delays the
 * record. `noteInput` is deliberately blind to whether a program echoes at all,
 * so only a run of keystrokes — a user who was typing — can expire loudly: one
 * lone key into a password prompt or copy-mode must not enter the journal.
 */
export const ECHO_TIMEOUT_MS = 5_000;
export const ECHO_TIMEOUT_MIN_INPUTS = 2;

/** A stalled pane produces one line per episode, not one per keystroke. */
export const ECHO_INCIDENT_INTERVAL_MS = 10_000;

/**
 * How long a physical key vouches for the input that follows it.
 *
 * xterm's `onData` also fires for the terminal's *automatic* replies to program
 * queries — cursor-position reports, device attributes — which TUIs ask for
 * constantly. Those are not typing, and measuring them journalled lag for a
 * machine nobody was sitting at. A measurement therefore only starts when a
 * real keydown happened moments earlier; the window is wide enough to cover the
 * keydown → onData hop plus an IME or composition detour, and far too narrow to
 * be reached by a synthetic reply arriving out of the blue.
 */
export const ECHO_KEY_RECENCY_MS = 250;

export type EchoLagIncident =
  | { kind: "input.echoLag"; paneId: string; lagMs: number; inputCount: number }
  | { kind: "input.echoTimeout"; paneId: string; waitedMs: number; inputCount: number };

export interface EchoLagProbeOptions {
  onIncident: (incident: EchoLagIncident) => void;
  /** Injected so tests can drive the clock independently of the timers. */
  now?: () => number;
}

export interface EchoLagProbe {
  /** A physical key was pressed in `paneId`, which is what makes input real. */
  noteKey(paneId: string): void;
  /** One batch of input bytes was dispatched to `paneId`. */
  noteInput(paneId: string): void;
  /** Output, a seed, or restored content was delivered to `paneId`. */
  noteOutput(paneId: string): void;
  dispose(): void;
}

interface PendingEcho {
  t0: number;
  inputCount: number;
  timer: ReturnType<typeof setTimeout>;
}

export function createEchoLagProbe({ onIncident, now = () => Date.now() }: EchoLagProbeOptions): EchoLagProbe {
  const pending = new Map<string, PendingEcho>();
  const lastIncidentAt = new Map<string, number>();
  const lastKeyAt = new Map<string, number>();

  const report = (incident: EchoLagIncident): void => {
    const at = now();
    const previous = lastIncidentAt.get(incident.paneId);
    if (previous !== undefined && at - previous < ECHO_INCIDENT_INTERVAL_MS) return;
    lastIncidentAt.set(incident.paneId, at);
    onIncident(incident);
  };

  return {
    noteKey(paneId) {
      lastKeyAt.set(paneId, now());
    },
    noteInput(paneId) {
      const open = pending.get(paneId);
      // A measurement already running is the one the user is waiting on: the
      // bytes sent during it are part of the same wait, not new ones — and that
      // includes the terminal's own replies, which are exactly the traffic a
      // stalled pane produces while the user waits.
      if (open) {
        open.inputCount += 1;
        return;
      }
      const lastKey = lastKeyAt.get(paneId);
      // Nothing a program asked the terminal for may open a measurement. Only a
      // key the user actually pressed can.
      if (lastKey === undefined || now() - lastKey > ECHO_KEY_RECENCY_MS) return;
      const t0 = now();
      const timer = setTimeout(() => {
        const expired = pending.get(paneId);
        pending.delete(paneId);
        if (!expired || expired.inputCount < ECHO_TIMEOUT_MIN_INPUTS) return;
        report({
          kind: "input.echoTimeout",
          paneId,
          waitedMs: now() - expired.t0,
          inputCount: expired.inputCount,
        });
      }, ECHO_TIMEOUT_MS);
      pending.set(paneId, { t0, inputCount: 1, timer });
    },
    noteOutput(paneId) {
      const open = pending.get(paneId);
      if (!open) return;
      pending.delete(paneId);
      clearTimeout(open.timer);
      const lagMs = now() - open.t0;
      if (lagMs > ECHO_LAG_THRESHOLD_MS) {
        report({ kind: "input.echoLag", paneId, lagMs, inputCount: open.inputCount });
      }
    },
    dispose() {
      for (const open of pending.values()) clearTimeout(open.timer);
      pending.clear();
      lastIncidentAt.clear();
      lastKeyAt.clear();
    },
  };
}
