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

  it("marks only the exact routable pane", () => {
    const direct = agent({ paneId: "%outer", attentionGeneration: 7, seenGeneration: 2 });
    expect(agentsMatchingFocusedPane([direct], "%outer")).toEqual([{ agentId: "agent-1", attentionGeneration: "7" }]);
    expect(agentsMatchingFocusedPane([direct], "%other")).toEqual([]);
  });
});
