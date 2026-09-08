import { describe, expect, it } from "vitest";
import { agentWindowName, agentWorkspaceName, compareAgents, displayState, needsAttention, sortedAgents, agentPinned, pinnedDividers, RECENT_WINDOW_MS, summarizeWaiting, waitingCount, waitingState } from "./selectors";
import type { Agent, SessionState } from "./sessionStore";

function agent(overrides: Partial<Agent> & { id: string }): Agent {
  return {
    adapterId: "codex",
    nativeSessionId: "",
    displayName: overrides.id,
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 1000,
    lifecycleChangedAtMs: 1000,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "fallback-ws", windowId: "@1", windowNameFallback: "fallback-win", paneId: "%1", paneIndexFallback: 0 },
    ...overrides,
  };
}

describe("display state (§8.2, ported from the desktop)", () => {
  it.each<[Partial<Agent>, string]>([
    [{ lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n }, "done"],
    [{ lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 2n }, "idle"],
    [{ lifecycle: "idle", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n }, "idle"],
    [{ lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n }, "blocked"],
    [{ lifecycle: "working", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n }, "working"],
    [{ lifecycle: "unknown" }, "unknown"],
  ])("case %#: %s", (overrides, expected) => {
    expect(displayState(agent({ id: "a", ...overrides }))).toBe(expected);
  });

  it("needsAttention is attentionGeneration > seenGeneration", () => {
    expect(needsAttention(agent({ id: "a", attentionGeneration: 3n, seenGeneration: 2n }))).toBe(true);
    expect(needsAttention(agent({ id: "a", attentionGeneration: 2n, seenGeneration: 2n }))).toBe(false);
  });
});

describe("agents list order (§8.2)", () => {
  it("orders blocked+attention, blocked, working, recent (done and fresh idle), idle, unknown, then gone agents", () => {
    const agents: Record<string, Agent> = {};
    for (const a of [
      agent({ id: "unknown", lifecycle: "unknown" }),
      agent({ id: "gone-blocked", lifecycle: "blocked", attentionGeneration: 5n, present: false }),
      agent({ id: "idle", lifecycle: "idle" }),
      agent({ id: "working", lifecycle: "working" }),
      agent({ id: "done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n }),
      agent({ id: "blocked-seen", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 2n }),
      agent({ id: "blocked-new", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n }),
    ]) agents[a.id] = a;
    // `now` far past every lifecycleChangedAtMs (1000), so plain idle is old.
    expect(sortedAgents({ agents, sessions: {}, windows: {} }, 1_000_000_000).map((a) => a.id)).toEqual([
      "blocked-new", "blocked-seen", "working", "done", "idle", "unknown", "gone-blocked",
    ]);
  });

  it("keeps a freshly idle agent in the recent bucket, clocked from the seen completion when there is one", () => {
    const now = 1_000_000_000;
    const agents: Record<string, Agent> = {};
    for (const a of [
      agent({ id: "old-idle", lifecycle: "idle", lifecycleChangedAtMs: now - RECENT_WINDOW_MS - 1 }),
      agent({ id: "fresh-idle", lifecycle: "idle", lifecycleChangedAtMs: now - 1000 }),
      // Finished long ago, acknowledged just now: recent by the seen clock.
      agent({
        id: "seen-done", lifecycle: "idle", attentionKind: "completed",
        attentionGeneration: 2n, seenGeneration: 2n,
        lifecycleChangedAtMs: now - RECENT_WINDOW_MS - 1, attentionSeenAtMs: now - 1000,
      }),
      agent({ id: "working", lifecycle: "working", lifecycleChangedAtMs: now - 5000 }),
    ]) agents[a.id] = a;
    expect(sortedAgents({ agents, sessions: {}, windows: {} }, now).map((a) => a.id)).toEqual([
      "working", "fresh-idle", "seen-done", "old-idle",
    ]);
  });

  it("breaks ties by the last lifecycle change descending, not by every hook update, then name", () => {
    const older = agent({ id: "older", lifecycleChangedAtMs: 1, updatedAtMs: 9 });
    const newer = agent({ id: "newer", lifecycleChangedAtMs: 2, updatedAtMs: 3 });
    expect([older, newer].sort(compareAgents).map((a) => a.id)).toEqual(["newer", "older"]);
    const a = agent({ id: "a", displayName: "Alpha" });
    const b = agent({ id: "b", displayName: "Beta" });
    expect([b, a].sort(compareAgents).map((a) => a.id)).toEqual(["a", "b"]);
  });
});

describe("workspace and window names", () => {
  const state: Pick<SessionState, "sessions" | "windows"> = {
    sessions: { $1: { id: "$1", name: "muxflow", windowCount: 1, order: 0, pinned: false } },
    windows: { "@1": { id: "@1", sessionId: "$1", index: 0, name: "✳ Fix tests", active: true, pinned: false } },
  };
  it("prefers live topology names, stripped of status glyphs, over route fallbacks", () => {
    expect(agentWorkspaceName(state, agent({ id: "a" }))).toBe("muxflow");
    expect(agentWindowName(state, agent({ id: "a" }))).toBe("Fix tests");
    const unrouted = agent({ id: "b", route: { sessionId: "$9", sessionNameFallback: "old-ws", windowId: "@9", windowNameFallback: "⠋ old-win", paneId: "", paneIndexFallback: 0 } });
    expect(agentWorkspaceName(state, unrouted)).toBe("old-ws");
    expect(agentWindowName(state, unrouted)).toBe("old-win");
  });
});

describe("pinned block (host-owned pins, desktop sidebar rule)", () => {
  const sessions = {
    $1: { id: "$1", name: "a", windowCount: 1, order: 0, pinned: false },
    $2: { id: "$2", name: "b", windowCount: 1, order: 1, pinned: true },
  };
  const windows = {
    "@1": { id: "@1", sessionId: "$1", index: 0, name: "w1", active: true, pinned: false },
    "@2": { id: "@2", sessionId: "$1", index: 1, name: "w2", active: false, pinned: true },
    "@3": { id: "@3", sessionId: "$2", index: 0, name: "w3", active: true, pinned: false },
  };
  const route = (sessionId: string, windowId: string) => ({ sessionId, sessionNameFallback: "", windowId, windowNameFallback: "", paneId: "%1", paneIndexFallback: 0 });

  it("an agent is pinned when its workspace or its tab is", () => {
    expect(agentPinned({ sessions, windows }, agent({ id: "plain", route: route("$1", "@1") }))).toBe(false);
    expect(agentPinned({ sessions, windows }, agent({ id: "tab", route: route("$1", "@2") }))).toBe(true);
    expect(agentPinned({ sessions, windows }, agent({ id: "ws", route: route("$2", "@3") }))).toBe(true);
  });

  it("leads pinned rows only within their own status", () => {
    const NOW = 1_000_000_000;
    const agents: Record<string, Agent> = {};
    for (const a of [
      agent({ id: "blocked-plain", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n, route: route("$1", "@1") }),
      agent({ id: "idle-pinned", lifecycle: "idle", route: route("$2", "@3") }),
      agent({ id: "working-plain", lifecycle: "working", route: route("$1", "@1") }),
      agent({ id: "working-pinned", lifecycle: "working", route: route("$1", "@2") }),
    ]) agents[a.id] = a;
    expect(sortedAgents({ agents, sessions, windows }, NOW).map((a) => a.id)).toEqual([
      "blocked-plain", "working-pinned", "working-plain", "idle-pinned",
    ]);
  });

  it("places a Pinned divider at 0 and a trailing divider after the block, or none", () => {
    expect(pinnedDividers([])).toEqual({ pinnedAt: null, restAt: null });
    expect(pinnedDividers([false, false])).toEqual({ pinnedAt: null, restAt: null });
    expect(pinnedDividers([true, true])).toEqual({ pinnedAt: 0, restAt: null });
    expect(pinnedDividers([true, false, false])).toEqual({ pinnedAt: 0, restAt: 1 });
  });
});

describe("waiting (the desktop's needsAttention(state): bell, unread badge, workspace count)", () => {
  const seenBlocked = agent({ id: "seen-blocked", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 2n });
  const unreadDone = agent({ id: "unread-done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n });
  const seenDone = agent({ id: "seen-done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 2n });
  const goneBlocked = agent({ id: "gone", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n, present: false });

  it("waits while blocked whether or not it was looked at; a completion waits only until it is", () => {
    expect(waitingState(seenBlocked)).toBe("blocked");
    expect(waitingState(unreadDone)).toBe("done");
    expect(waitingState(seenDone)).toBeUndefined();
    expect(waitingState(agent({ id: "w" }))).toBeUndefined();
    expect(waitingState(goneBlocked)).toBeUndefined();
  });

  it("counts every waiting agent and lets blocked outrank done as the loudest", () => {
    expect(summarizeWaiting([unreadDone, seenDone])).toEqual({ count: 1, loudest: "done" });
    expect(summarizeWaiting([unreadDone, seenBlocked, goneBlocked])).toEqual({ count: 2, loudest: "blocked" });
    expect(summarizeWaiting([])).toEqual({ count: 0, loudest: undefined });
    expect(waitingCount({ agents: { a: unreadDone, b: seenBlocked, c: seenDone, d: goneBlocked } })).toBe(2);
  });
});
