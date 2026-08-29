import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RECENT_IDLE_WINDOW_MILLIS } from "./agentsList";
import { agent } from "./testFixtures";
import type { AgentRecord } from "./types";
import { useRecentIdleClock } from "./useRecentIdleClock";

let revision = -1;

function Harness({ agents, enabled = true }: { agents: readonly AgentRecord[]; enabled?: boolean }) {
  revision = useRecentIdleClock(agents, enabled);
  return null;
}

describe("Recent idle deadline clock", () => {
  beforeEach(() => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-28T12:00:00Z"));
    revision = -1;
  });

  afterEach(() => vi.useRealTimers());

  it("ticks at the nearest Recent expiration without polling", async () => {
    const now = Date.now();
    const rows = [
      agent({ id: "later", lifecycle: "idle", lifecycleChangedAt: now - 60 * 60 * 1_000 }),
      agent({ id: "sooner", lifecycle: "idle", lifecycleChangedAt: now - 3 * 60 * 60 * 1_000 }),
    ];
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness agents={rows} />); });
    expect(revision).toBe(0);

    await act(async () => { vi.advanceTimersByTime(60 * 60 * 1_000 - 1); });
    expect(revision).toBe(0);
    await act(async () => { vi.advanceTimersByTime(1); });
    expect(revision).toBe(1);

    // The first row is now old; the same one-shot scheduler advances at the
    // remaining row's deadline rather than waking on an interval.
    await act(async () => { vi.advanceTimersByTime(2 * 60 * 60 * 1_000); });
    expect(revision).toBe(2);
    await act(async () => renderer!.unmount());
  });

  it("does not run while workspace ordering is selected", async () => {
    const rows = [agent({
      lifecycle: "idle",
      lifecycleChangedAt: Date.now() - 3 * 60 * 60 * 1_000,
    })];
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness agents={rows} enabled={false} />); });
    await act(async () => { vi.advanceTimersByTime(RECENT_IDLE_WINDOW_MILLIS); });
    expect(revision).toBe(0);
    await act(async () => renderer!.unmount());
  });
});
