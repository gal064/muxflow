import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createGridMismatchProbe,
  GRID_MISMATCH_INCIDENT_INTERVAL_MS,
  GRID_MISMATCH_SUSTAIN_MS,
  type GridMismatchIncident,
} from "./gridMismatchProbe";

/** The injected clock is advanced with the timers so a fire can be timed. */
function probeWithClock() {
  const incidents: GridMismatchIncident[] = [];
  let clock = 0;
  const probe = createGridMismatchProbe({
    onIncident: (incident) => incidents.push(incident),
    now: () => clock,
  });
  return {
    incidents,
    probe,
    advance(ms: number) {
      clock += ms;
      vi.advanceTimersByTime(ms);
    },
    /** What a reconcile does: applies tmux's grid, having measured the box. */
    note(tmux: [number, number], measured: [number, number] | undefined, paneId = "%1") {
      probe.noteGrids(
        paneId,
        { columns: tmux[0], rows: tmux[1] },
        measured && { columns: measured[0], rows: measured[1] },
      );
    },
  };
}

describe("createGridMismatchProbe", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("records a disagreement that outlived any answered round trip", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42]);
    advance(GRID_MISMATCH_SUSTAIN_MS - 1);
    expect(incidents).toEqual([]);
    advance(1);
    expect(incidents).toEqual([{
      kind: "pane.gridMismatch",
      paneId: "%1",
      tmuxColumns: 106,
      tmuxRows: 48,
      measuredColumns: 106,
      measuredRows: 42,
      msSustained: GRID_MISMATCH_SUSTAIN_MS,
    }]);
    // One episode, one line: the timer is spent and nothing re-arms it.
    advance(GRID_MISMATCH_SUSTAIN_MS * 10);
    expect(incidents).toHaveLength(1);
    probe.dispose();
  });

  it("says nothing about a disagreement the round trip answered", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42]);
    advance(8_000);
    // tmux answered: the grid the terminal renders at is now the box's own.
    note([106, 42], [106, 42]);
    advance(GRID_MISMATCH_SUSTAIN_MS * 2);
    expect(incidents).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
    probe.dispose();
  });

  it("leaves the routine one-cell difference alone, forever", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    // A divider column between two splits. Every healthy pane looks like this.
    for (let i = 0; i < 20; i++) {
      note([105, 42], [106, 42]);
      advance(GRID_MISMATCH_SUSTAIN_MS);
    }
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("keeps one episode's clock running while its numbers wobble", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42]);
    advance(6_000);
    // Still disagreeing, just by a different amount — a remeasurement, not a
    // new episode, so the deadline stays where the first observation put it.
    note([106, 47], [106, 42]);
    advance(4_000);
    expect(incidents).toEqual([{
      kind: "pane.gridMismatch",
      paneId: "%1",
      tmuxColumns: 106,
      tmuxRows: 47,
      measuredColumns: 106,
      measuredRows: 42,
      msSustained: GRID_MISMATCH_SUSTAIN_MS,
    }]);
    probe.dispose();
  });

  it("ignores a box that measured nothing usable", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42]);
    // A pane whose box has no size yet cannot disagree with anything.
    note([106, 48], undefined);
    advance(GRID_MISMATCH_SUSTAIN_MS * 2);
    expect(incidents).toEqual([]);
    probe.dispose();
  });

  it("emits once per pane per rate-limit window", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42]);
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents).toHaveLength(1);

    // A second episode inside the window: armed, sustained, and swallowed.
    note([106, 48], [106, 42]);
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents).toHaveLength(1);

    advance(GRID_MISMATCH_INCIDENT_INTERVAL_MS);
    note([106, 48], [106, 40]);
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents).toHaveLength(2);
    expect(incidents[1]).toMatchObject({ measuredRows: 40 });
    probe.dispose();
  });

  it("charges the rate limit per pane, so one pane cannot silence another", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42], "%1");
    note([106, 48], [106, 42], "%2");
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents.map((incident) => incident.paneId)).toEqual(["%1", "%2"]);
    probe.dispose();
  });

  it("forgets a pane that was cleared", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42], "%1");
    note([106, 48], [106, 42], "%2");
    probe.clear("%1");
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents.map((incident) => incident.paneId)).toEqual(["%2"]);
    // And a cleared pane arms again on the next observation of the same state.
    probe.clear("%2");
    note([106, 48], [106, 42], "%1");
    advance(GRID_MISMATCH_SUSTAIN_MS);
    expect(incidents.map((incident) => incident.paneId)).toEqual(["%2", "%1"]);
    probe.dispose();
  });

  it("clears every armed timer on dispose", () => {
    const { advance, incidents, note, probe } = probeWithClock();
    note([106, 48], [106, 42], "%1");
    note([80, 24], [80, 40], "%2");
    probe.dispose();
    advance(GRID_MISMATCH_SUSTAIN_MS * 2);
    expect(vi.getTimerCount()).toBe(0);
    expect(incidents).toEqual([]);
  });
});
