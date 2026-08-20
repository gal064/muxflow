import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createEchoLagProbe,
  ECHO_INCIDENT_INTERVAL_MS,
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
  };
}

describe("createEchoLagProbe", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("records the echo that came back too late", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 120);
    probe.noteOutput("%1");
    expect(incidents).toEqual([
      { kind: "input.echoLag", paneId: "%1", lagMs: ECHO_LAG_THRESHOLD_MS + 120, inputCount: 1 },
    ]);
    probe.dispose();
  });

  it("counts every keystroke the user typed into one wait", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
    advance(50);
    probe.noteInput("%1");
    advance(ECHO_LAG_THRESHOLD_MS);
    probe.noteOutput("%1");
    expect(incidents).toEqual([
      { kind: "input.echoLag", paneId: "%1", lagMs: ECHO_LAG_THRESHOLD_MS + 50, inputCount: 2 },
    ]);
    probe.dispose();
  });

  it("stays silent for an echo that arrived in time", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
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
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
    probe.noteInput("%1");
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
    const { advance, incidents, probe } = probeWithClock();
    // A password prompt or copy-mode key echoes nothing, and journalling that
    // would bury the real stalls under one line per unechoed key.
    probe.noteInput("%1");
    advance(ECHO_TIMEOUT_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("records one incident per pane per rate-limit window", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    probe.noteInput("%1");
    probe.noteInput("%2");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    // Another pane is another episode, so the limit is per pane and not global.
    probe.noteOutput("%2");
    expect(incidents.map((incident) => incident.paneId)).toEqual(["%1", "%2"]);
    advance(ECHO_INCIDENT_INTERVAL_MS);
    probe.noteInput("%1");
    advance(ECHO_LAG_THRESHOLD_MS + 1);
    probe.noteOutput("%1");
    expect(incidents).toHaveLength(3);
    expect(incidents[2]).toMatchObject({ kind: "input.echoLag", paneId: "%1" });
    probe.dispose();
  });

  it("clears every armed timer on dispose", () => {
    const { advance, incidents, probe } = probeWithClock();
    probe.noteInput("%1");
    probe.noteInput("%1");
    probe.noteInput("%2");
    probe.noteInput("%2");
    probe.dispose();
    advance(ECHO_TIMEOUT_MS * 2);
    expect(vi.getTimerCount()).toBe(0);
    expect(incidents).toEqual([]);
  });
});
