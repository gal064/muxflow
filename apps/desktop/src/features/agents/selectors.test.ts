import { describe, expect, it } from "vitest";
import { agentsMatchingFocusedPane, deriveAgentRollups, displayState } from "./selectors";
import { agent } from "./testFixtures";

describe("agent selectors", () => {
  it("represents done only as idle with an unseen persisted completion", () => {
    expect(displayState(agent({ lifecycle: "idle", attentionGeneration: 4, attentionKind: "completed", seenGeneration: 3 }))).toBe("done");
    expect(displayState(agent({ lifecycle: "idle", attentionGeneration: 4, attentionKind: "completed", seenGeneration: 4 }))).toBe("idle");
    expect(displayState(agent({ lifecycle: "idle", attentionGeneration: 4, attentionKind: "blocked", seenGeneration: 3 }))).toBe("idle");
    expect(displayState(agent({ lifecycle: "idle", attentionGeneration: 4, seenGeneration: 3 }))).toBe("idle");
  });

  it("rolls authoritative priority agent to pane, window, and workspace", () => {
    const agents = [
      agent({ id: "working", lifecycle: "working", paneId: "%1" }),
      agent({ id: "done", lifecycle: "idle", paneId: "%2", attentionGeneration: 2, attentionKind: "completed", seenGeneration: 1 }),
      agent({ id: "blocked", lifecycle: "blocked", paneId: "%2", attentionGeneration: 3 }),
    ];
    const result = deriveAgentRollups(agents);
    expect(result.byPane.get("%1")?.state).toBe("working");
    expect(result.byPane.get("%2")).toMatchObject({ state: "blocked", blocked: 1, done: 1 });
    expect(result.byWindow.get("@1")?.state).toBe("blocked");
    expect(result.byWorkspace.get("$1")?.total).toBe(3);
  });

  it("takes the adapter from the same agent the state came from", () => {
    // Two agents, one window: the group reports one state, so it must report
    // that agent's adapter and not the other's — an icon and a state describing
    // different agents would be a single mark telling two stories.
    const loudestLast = deriveAgentRollups([
      agent({ id: "quiet", adapterId: "claude-code", lifecycle: "idle" }),
      agent({ id: "loud", adapterId: "codex", lifecycle: "blocked" }),
    ]);
    expect(loudestLast.byWindow.get("@1")).toMatchObject({ state: "blocked", adapterId: "codex" });
    // Same pair, other order: the winner is the state, not the position.
    const loudestFirst = deriveAgentRollups([
      agent({ id: "loud", adapterId: "codex", lifecycle: "blocked" }),
      agent({ id: "quiet", adapterId: "claude-code", lifecycle: "idle" }),
    ]);
    expect(loudestFirst.byWindow.get("@1")).toMatchObject({ state: "blocked", adapterId: "codex" });
    // A tie goes to the first agent in the order the caller supplied.
    const tied = deriveAgentRollups([
      agent({ id: "first", adapterId: "claude-code", lifecycle: "working" }),
      agent({ id: "second", adapterId: "codex", lifecycle: "working" }),
    ]);
    expect(tied.byWindow.get("@1")).toMatchObject({ state: "working", adapterId: "claude-code" });
  });

  it("marks only the exact routable pane", () => {
    const direct = agent({ paneId: "%outer", attentionGeneration: 7, seenGeneration: 2 });
    expect(agentsMatchingFocusedPane([direct], "%outer")).toEqual([{ agentId: "agent-1", attentionGeneration: "7" }]);
    expect(agentsMatchingFocusedPane([direct], "%other")).toEqual([]);
  });
});
