import { describe, expect, it } from "vitest";

import type { Agent } from "../../store/sessionStore";
import { colors } from "../../ui/tokens";
import { agentDisplayName, agentTitle, waitingColor, waitingInSession, waitingLabel } from "./agentViews";

function agent(overrides: Partial<Agent> & { id: string; sessionId: string }): Agent {
  const { sessionId, ...rest } = overrides;
  return {
    adapterId: "codex",
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
    route: { sessionId, sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId: "%1", paneIndexFallback: 0 },
    ...rest,
  };
}

describe("Workspaces tab attention (§9.3.2, the desktop's workspace row)", () => {
  const agents = {
    blocked: agent({ id: "blocked", sessionId: "$1", lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 2n }),
    done: agent({ id: "done", sessionId: "$1", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n }),
    doneElsewhere: agent({ id: "done-2", sessionId: "$2", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n }),
    working: agent({ id: "working", sessionId: "$2" }),
    goneBlocked: agent({ id: "gone", sessionId: "$3", lifecycle: "blocked", present: false }),
  };

  it("counts a workspace's waiting agents and paints its loudest: red for blocked, green for a completion", () => {
    expect(waitingInSession({ agents }, "$1")).toEqual({ count: 2, loudest: "blocked" });
    expect(waitingInSession({ agents }, "$2")).toEqual({ count: 1, loudest: "done" });
    expect(waitingInSession({ agents }, "$3")).toEqual({ count: 0, loudest: undefined });
    expect(waitingColor("blocked")).toBe(colors.danger);
    expect(waitingColor("done")).toBe(colors.ok);
  });

  it("words the count the way the desktop's row label does", () => {
    expect(waitingLabel(1)).toBe("1 waiting");
    expect(waitingLabel(3)).toBe("3 waiting");
  });
});

describe("agentTitle (§9.3.1 line 1: the tab, the desktop's agentSessionLabel)", () => {
  const adapters = [{ id: "codex", displayName: "Codex CLI", hookWiring: "wired" as const }];
  const windows = { "@1": { id: "@1", sessionId: "$1", index: 0, name: "⠦ Fix tests", active: false, pinned: false } };
  const routed = (overrides: Partial<Agent> = {}) =>
    agent({ id: "a", sessionId: "$1", route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "old name", paneId: "%1", paneIndexFallback: 3 }, ...overrides });

  it("names the live tab with its ticker stripped, over the assigned name", () => {
    expect(agentTitle({ windows, adapters }, routed({ displayName: "Codex" }))).toBe("Fix tests");
  });

  it("falls back to the retained window name, then the assigned name, then the adapter", () => {
    expect(agentTitle({ windows: {}, adapters }, routed({ displayName: "Codex" }))).toBe("old name");
    const noWindow = routed({ displayName: "Nightly triage", route: { ...routed().route, windowNameFallback: "" } });
    expect(agentTitle({ windows: {}, adapters }, noWindow)).toBe("Nightly triage");
    expect(agentTitle({ windows: { "@1": { ...windows["@1"], name: "codex" } }, adapters }, routed({ displayName: "" }))).toBe("Codex CLI");
  });

  it("says where a gone agent with neither a window nor a name was", () => {
    const bare = routed({ displayName: "", present: false, route: { ...routed().route, windowNameFallback: "" } });
    expect(agentTitle({ windows: {}, adapters }, bare)).toBe("Codex CLI in pane 3");
    expect(agentTitle({ windows: {}, adapters: [] }, bare)).toBe("codex in pane 3");
  });
});

describe("agentDisplayName (§9.4 line 2: who, when the row is already the window)", () => {
  it("is the assigned name, else the adapter's", () => {
    const adapters = [{ id: "codex", displayName: "Codex CLI", hookWiring: "wired" as const }];
    expect(agentDisplayName({ adapters }, agent({ id: "a", sessionId: "$1", displayName: "Nightly triage" }))).toBe("Nightly triage");
    expect(agentDisplayName({ adapters }, agent({ id: "a", sessionId: "$1", displayName: " " }))).toBe("Codex CLI");
    expect(agentDisplayName({ adapters: [] }, agent({ id: "a", sessionId: "$1", displayName: "" }))).toBe("codex");
  });
});
