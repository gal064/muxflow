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

/**
 * Above the point where an echo stops feeling instant on a remote link.
 *
 * It sat at 100 for a measurement campaign: the histograms had established the
 * healthy baseline (16-32ms typical), and the open question was which leg the
 * 100-300ms spikes lived in — a question only per-spike records with link
 * context attached could answer. That campaign has concluded. Every leg
 * measured flat during the spikes (desktop send, Rust queue, daemon, network,
 * tmux), which attributes them to the agent process's own repaint while it is
 * busy — not to anything this app can fix or needs to keep watching. So the
 * threshold returns to "worth a journal line" territory at 250.
 *
 * Nothing is lost below it: every echo that comes back still reaches `onSample`
 * and lands in the `inputLatencyStats` histograms, so the full latency
 * distribution is intact. What 250 drops is only the per-spike journal lines
 * for a range now proven benign.
 */
export const ECHO_LAG_THRESHOLD_MS = 250;

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

/** One reading of the native link reader's cumulative counters. */
export interface EchoLinkCounters {
  bytesRead: number;
  framesRead: number;
}

/**
 * How much other traffic the echo waited behind: what the native reader took
 * off the ssh stream between the keystroke and its echo.
 *
 * This is the same head-of-line evidence `perf.timeline` reports for a tmux
 * action, for the one measurement that lives entirely in the renderer. Absent
 * whenever the counters were not sampled — an unmeasured process, or an echo
 * inside the sampling interval below.
 */
export interface EchoAhead {
  bytesAhead?: number;
  framesAhead?: number;
}

/**
 * At most one sampled measurement per pane per this interval.
 *
 * Sampling costs two native calls per measurement, and the fast baseline the
 * campaign needs is a distribution, not every keystroke: four samples a second
 * describe it and cannot themselves become the lag being measured.
 */
export const ECHO_SAMPLE_INTERVAL_MS = 250;

/** One completed echo, sampled: the compact baseline record. */
export interface EchoRecord extends EchoAhead {
  paneId: string;
  sentAt: number;
  echoAt: number;
  lagMs: number;
  inputCount: number;
}

export type EchoLagIncident =
  | ({ kind: "input.echoLag"; paneId: string; lagMs: number; inputCount: number } & EchoAhead)
  | ({ kind: "input.echoTimeout"; paneId: string; waitedMs: number; inputCount: number } & EchoAhead);

export interface EchoLagProbeOptions {
  onIncident: (incident: EchoLagIncident) => void;
  /**
   * Reads the native reader's cumulative counters, or returns `undefined` when
   * there is nothing to read — which is every unmeasured process, and is what
   * keeps this whole path free in a normal launch.
   */
  sampleLinkCounters?: () => Promise<EchoLinkCounters | undefined> | undefined;
  /** Every sampled echo, fast or slow. The tail is `onIncident`; this is the baseline. */
  onEcho?: (echo: EchoRecord) => void;
  /**
   * Every completed measurement, outlier or not.
   *
   * The incident above is the tail; this is the distribution. `inputLatencyStats`
   * aggregates these into the one histogram record that says how a normal
   * keystroke behaved, which is what an outlier has to be compared against. An
   * abandoned measurement is deliberately not sampled: it never echoed, so it
   * has no round trip to contribute.
   */
  onSample?: (paneId: string, lagMs: number) => void;
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
  /**
   * Forgets every open measurement and every per-pane memory. Pane ids repeat
   * across hosts, so a host switch must not let the new host's `%1` painting
   * close a round trip the old host's `%1` started.
   */
  reset(): void;
  dispose(): void;
}

interface PendingEcho {
  t0: number;
  inputCount: number;
  timer: ReturnType<typeof setTimeout>;
  /** The counters as of the keystroke, when this measurement was sampled. */
  before?: Promise<EchoLinkCounters | undefined>;
}

export function createEchoLagProbe({
  onIncident,
  onSample,
  onEcho,
  sampleLinkCounters,
  now = () => Date.now(),
}: EchoLagProbeOptions): EchoLagProbe {
  const pending = new Map<string, PendingEcho>();
  const lastIncidentAt = new Map<string, number>();
  const lastKeyAt = new Map<string, number>();
  const lastSampleAt = new Map<string, number>();

  /**
   * The second reading has to be taken now, at the echo — awaiting the first
   * one before asking for it would fold this call's own latency into the
   * difference the record reports.
   */
  const ahead = async (before: Promise<EchoLinkCounters | undefined>): Promise<EchoAhead> => {
    const after = await Promise.all([before, sampleLinkCounters?.()]).then(
      ([start, end]) => (start && end ? { start, end } : undefined),
    ).catch(() => undefined);
    if (!after) return {};
    return {
      bytesAhead: Math.max(0, after.end.bytesRead - after.start.bytesRead),
      framesAhead: Math.max(0, after.end.framesRead - after.start.framesRead),
    };
  };

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
        const waitedMs = now() - expired.t0;
        const timeout = { kind: "input.echoTimeout" as const, paneId, waitedMs, inputCount: expired.inputCount };
        if (!expired.before) {
          report(timeout);
          return;
        }
        void ahead(expired.before)
          .then((counters) => report({ ...timeout, ...counters }))
          .catch(() => report(timeout));
      }, ECHO_TIMEOUT_MS);
      const sampledAt = lastSampleAt.get(paneId);
      const before = sampledAt === undefined || t0 - sampledAt >= ECHO_SAMPLE_INTERVAL_MS
        ? sampleLinkCounters?.()
        : undefined;
      if (before) lastSampleAt.set(paneId, t0);
      pending.set(paneId, { t0, inputCount: 1, timer, before });
    },
    noteOutput(paneId) {
      const open = pending.get(paneId);
      if (!open) return;
      pending.delete(paneId);
      clearTimeout(open.timer);
      const echoAt = now();
      const lagMs = echoAt - open.t0;
      onSample?.(paneId, lagMs);
      const outlier = lagMs > ECHO_LAG_THRESHOLD_MS;
      if (!open.before) {
        if (outlier) report({ kind: "input.echoLag", paneId, lagMs, inputCount: open.inputCount });
        return;
      }
      // A measurement that cannot read its counters still reports the lag: a
      // diagnostic must never fail the thing it is describing.
      void ahead(open.before)
        .catch(() => ({}) as EchoAhead)
        .then((counters) => {
          onEcho?.({ paneId, sentAt: open.t0, echoAt, lagMs, inputCount: open.inputCount, ...counters });
          if (outlier) {
            report({ kind: "input.echoLag", paneId, lagMs, inputCount: open.inputCount, ...counters });
          }
        });
    },
    reset() {
      this.dispose();
    },
    dispose() {
      for (const open of pending.values()) clearTimeout(open.timer);
      pending.clear();
      lastIncidentAt.clear();
      lastKeyAt.clear();
      lastSampleAt.clear();
    },
  };
}
