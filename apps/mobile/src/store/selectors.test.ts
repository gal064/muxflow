import { describe, expect, it } from "vitest";
import { agentWindowName, agentWorkspaceName, compareAgents, displayState, needsAttention, sortedAgents } from "./selectors";
import type { Agent, SessionState } from "./sessionStore";

function agent(overrides: Partial<Agent> & { id: string }): Agent {
  return {
    adapterId: "codex",
    displayName: overrides.id,
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 1000,
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
  it("orders blocked+attention, blocked, done, working, idle, unknown, then gone agents", () => {
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
    expect(sortedAgents({ agents }).map((a) => a.id)).toEqual([
      "blocked-new", "blocked-seen", "done", "working", "idle", "unknown", "gone-blocked",
    ]);
  });

  it("breaks ties by updatedAtMs descending, then name", () => {
    const older = agent({ id: "older", updatedAtMs: 1 });
    const newer = agent({ id: "newer", updatedAtMs: 2 });
    expect([older, newer].sort(compareAgents).map((a) => a.id)).toEqual(["newer", "older"]);
    const a = agent({ id: "a", displayName: "Alpha" });
    const b = agent({ id: "b", displayName: "Beta" });
    expect([b, a].sort(compareAgents).map((a) => a.id)).toEqual(["a", "b"]);
  });
});

describe("workspace and window names", () => {
  const state: Pick<SessionState, "sessions" | "windows"> = {
    sessions: { $1: { id: "$1", name: "muxflow", windowCount: 1, order: 0 } },
    windows: { "@1": { id: "@1", sessionId: "$1", index: 0, name: "✳ Fix tests", active: true } },
  };
  it("prefers live topology names, stripped of status glyphs, over route fallbacks", () => {
    expect(agentWorkspaceName(state, agent({ id: "a" }))).toBe("muxflow");
    expect(agentWindowName(state, agent({ id: "a" }))).toBe("Fix tests");
    const unrouted = agent({ id: "b", route: { sessionId: "$9", sessionNameFallback: "old-ws", windowId: "@9", windowNameFallback: "⠋ old-win", paneId: "", paneIndexFallback: 0 } });
    expect(agentWorkspaceName(state, unrouted)).toBe("old-ws");
    expect(agentWindowName(state, unrouted)).toBe("old-win");
  });
});
