import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createEchoLagProbe,
  ECHO_INCIDENT_INTERVAL_MS,
  ECHO_KEY_RECENCY_MS,
  ECHO_LAG_THRESHOLD_MS,
  ECHO_TIMEOUT_MS,
  type EchoLagIncident,
} from "./echoLagProbe";

/** The injected clock is advanced by hand so a timer can fire at any lag. */
function probeWithClock() {
  const incidents: EchoLagIncident[] = [];
  let clock = 0;
  const probe = createEchoLagProbe({ onIncident: (incident) => incidents.push(incident), now: () => clock });
  return {
    incidents,
    probe,
    advance(ms: number) {
      clock += ms;
      vi.advanceTimersByTime(ms);
    },
    /** What a human does: a key, then the bytes it produced. */
    type(paneId: string) {
      probe.noteKey(paneId);
      probe.noteInput(paneId);
    },
  };
}

describe("createEchoLagProbe", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("records the echo that came back too late", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 120);
    probe.noteOutput("%1");
    expect(incidents).toEqual([
      { kind: "input.echoLag", paneId: "%1", lagMs: ECHO_LAG_THRESHOLD_MS + 120, inputCount: 1 },
    ]);
    probe.dispose();
  });

  it("counts every keystroke the user typed into one wait", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    advance(50);
    type("%1");
    advance(ECHO_LAG_THRESHOLD_MS);
    probe.noteOutput("%1");
    expect(incidents).toEqual([
      { kind: "input.echoLag", paneId: "%1", lagMs: ECHO_LAG_THRESHOLD_MS + 50, inputCount: 2 },
    ]);
    probe.dispose();
  });

  it("stays silent for an echo that arrived in time", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    advance(ECHO_LAG_THRESHOLD_MS);
    probe.noteOutput("%1");
    // Resolved, so the pane's next keystroke starts a fresh measurement rather
    // than inheriting this one's start time.
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("ignores output for a pane nobody typed into", () => {
    const { advance, incidents, probe } = probeWithClock();
    advance(10_000);
    probe.noteOutput("%9");
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("reports a run of keystrokes that never echoed", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    type("%1");
    advance(ECHO_TIMEOUT_MS);
    expect(incidents).toEqual([
      { kind: "input.echoTimeout", paneId: "%1", waitedMs: ECHO_TIMEOUT_MS, inputCount: 2 },
    ]);
    // The measurement is gone: a late echo must not resolve it a second time.
    probe.noteOutput("%1");
    expect(incidents).toHaveLength(1);
    probe.dispose();
  });

  it("lets a lone keystroke expire silently", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    // A password prompt or copy-mode key echoes nothing, and journalling that
    // would bury the real stalls under one line per unechoed key.
    type("%1");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("records one incident per pane per rate-limit window", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    type("%1");
    type("%2");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    // Another pane is another episode, so the limit is per pane and not global.
    probe.noteOutput("%2");
    expect(incidents.map((incident) => incident.paneId)).toEqual(["%1", "%2"]);
    advance(ECHO_INCIDENT_INTERVAL_MS);
    type("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    expect(incidents).toHaveLength(3);
    expect(incidents[2]).toMatchObject({ kind: "input.echoLag", paneId: "%1" });
    probe.dispose();
  });

  it("starts nothing for input no key produced", () => {
    const { advance, incidents, probe } = probeWithClock();
    // xterm answers a program's cursor-position and device-attribute queries by
    // itself, and a TUI asks constantly. Twenty minutes away from the machine
    // used to fill the journal with these.
    probe.noteInput("%1");
    probe.noteInput("%1");
    probe.noteInput("%1");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
    probe.dispose();
  });

  it("starts a measurement for input a key just produced", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteKey("%1");
    advance(ECHO_KEY_RECENCY_MS);
    probe.noteInput("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    expect(incidents).toEqual([
      { kind: "input.echoLag", paneId: "%1", lagMs: ECHO_LAG_THRESHOLD_MS + 1, inputCount: 1 },
    ]);
    probe.dispose();
  });

  it("ignores a key too stale to have produced this input", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteKey("%1");
    advance(ECHO_KEY_RECENCY_MS + 1);
    probe.noteInput("%1");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("counts the terminal's own replies into a wait a key opened", () => {
    const { advance, incidents, probe } = probeWithClock();
    // The synthetic flood during a real stall is part of what the stall looks
    // like, so it belongs in the count — it just cannot start the measurement.
    probe.noteKey("%1");
    probe.noteInput("%1");
    advance(1_000);
    probe.noteInput("%1");
    probe.noteInput("%1");
    advance(ECHO_TIMEOUT_MS - 1_000);
    expect(incidents).toEqual([
      { kind: "input.echoTimeout", paneId: "%1", waitedMs: ECHO_TIMEOUT_MS, inputCount: 3 },
    ]);
    probe.dispose();
  });

  it("keeps the gate per pane", () => {
    const { advance, incidents, probe } = probeWithClock();
    // A key in one pane must not vouch for a query reply in another.
    probe.noteKey("%1");
    probe.noteInput("%2");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("forgets recorded keys on dispose", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteKey("%1");
    probe.dispose();
    probe.noteInput("%1");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("clears every armed timer on dispose", () => {
    const { advance, incidents, probe, type } = probeWithClock();
    type("%1");
    type("%1");
    type("%2");
    type("%2");
    probe.dispose();
    advance(ECHO_TIMEOUT_MS * 2);
    expect(vi.getTimerCount()).toBe(0);
    expect(incidents).toEqual([]);
  });
});
