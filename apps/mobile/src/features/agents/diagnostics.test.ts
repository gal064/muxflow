import { describe, expect, it } from "vitest";

import type { Agent } from "../../store/sessionStore";
import { logAgentTransitions } from "./diagnostics";

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "agent-a",
    adapterId: "codex",
    displayName: "Codex",
    lifecycle: "idle",
    attentionKind: "",
    stateGeneration: 4n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 0,
    lifecycleChangedAtMs: 0,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId: "%3", paneIndexFallback: 0 },
    ...overrides,
  };
}

describe("agent diagnostic summaries", () => {
  it("reports lifecycle and retirement edges with source and correlation ids", () => {
    const lines: string[] = [];
    logAgentTransitions(
      { "agent-a": agent() },
      { "agent-a": agent({ lifecycle: "working", stateGeneration: 5n }), "agent-b": agent({ id: "agent-b", route: { ...agent().route, paneId: "%4" } }) },
      "event:hook update",
      9n,
      (line) => lines.push(line),
    );
    expect(lines).toEqual([
      "agent.transition source=event:hook-update agent=agent-a pane=%3 present=true->true lifecycle=idle->working stateGeneration=5 topology=9",
      "agent.transition source=event:hook-update agent=agent-b pane=%4 present=false->true lifecycle=absent->idle stateGeneration=4 topology=9",
    ]);

    lines.length = 0;
    logAgentTransitions({ "agent-a": agent() }, {}, "refresh", 10n, (line) => lines.push(line));
    expect(lines[0]).toContain("agent=agent-a pane=%3 present=true->false lifecycle=idle->absent");
  });

  it("ignores repeated states and route-only changes", () => {
    const lines: string[] = [];
    logAgentTransitions(
      { "agent-a": agent() },
      { "agent-a": agent({ route: { ...agent().route, paneId: "%9" }, stateGeneration: 8n }) },
      "snapshot",
      2n,
      (line) => lines.push(line),
    );
    expect(lines).toEqual([]);
  });
});
