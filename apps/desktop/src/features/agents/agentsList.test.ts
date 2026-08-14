import { describe, expect, it } from "vitest";
import { buildAgentRows, jumpTarget, needsAttention, nextSortMode, unreadCount, type AgentLocation } from "./agentsList";
import { agent } from "./testFixtures";
import type { AgentRecord } from "./types";

const locations: Record<string, AgentLocation> = {
  "$1": { workspaceOrder: 0, workspaceName: "muxflow", tabIndex: 1 },
  "$2": { workspaceOrder: 1, workspaceName: "sampleco-e2e", tabIndex: 2 },
};
const locate = (record: AgentRecord): AgentLocation =>
  locations[record.sessionId] ?? { workspaceOrder: Number.MAX_SAFE_INTEGER, workspaceName: record.sessionName };

const working = agent({ id: "working", displayName: "claude", lifecycle: "working", updatedAt: 30 });
const blocked = agent({ id: "blocked", displayName: "codex", lifecycle: "blocked", sessionId: "$1", updatedAt: 20 });
const done = agent({ id: "done", displayName: "claude two", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4, seenGeneration: 2, sessionId: "$2", updatedAt: 10 });
const idle = agent({ id: "idle", displayName: "claude three", lifecycle: "idle", sessionId: "$2", updatedAt: 5 });

describe("agents section ordering", () => {
  it("ranks the inbox blocked > done-unread > working > idle", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "priority");
    expect(rows.map((row) => row.agent.id)).toEqual(["blocked", "done", "working", "idle"]);
    // The point of the ranking: a finished agent outranks a running one,
    // because the finished one is the one waiting on a human.
    expect(rows.findIndex((row) => row.agent.id === "done"))
      .toBeLessThan(rows.findIndex((row) => row.agent.id === "working"));
  });

  it("follows the workspace list, then tab order, in grouped mode", () => {
    const rows = buildAgentRows([done, idle, blocked, working], locate, () => true, "grouped");
    expect(rows.map((row) => row.location.workspaceName)).toEqual(["muxflow", "muxflow", "sampleco-e2e", "sampleco-e2e"]);
    // Within a workspace, ties fall back to the agent's own name so the list
    // does not reshuffle on every update.
    expect(rows.slice(2).map((row) => row.agent.displayName)).toEqual(["claude three", "claude two"]);
  });

  it("sends ⌘⇧U to the loudest reachable agent that actually wants something", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "grouped");
    expect(jumpTarget(rows)?.agent.id).toBe("blocked");
    // Ordering mode must not change where the jump lands.
    expect(jumpTarget(buildAgentRows([idle, working, done, blocked], locate, () => true, "priority"))?.agent.id).toBe("blocked");
    // An unroutable row is not a destination; the next one down is.
    expect(jumpTarget(rows.map((row) => row.agent.id === "blocked" ? { ...row, routable: false } : row))?.agent.id).toBe("done");
    // Nothing waiting means nothing to jump to, rather than "jump to whatever
    // sorted first".
    expect(jumpTarget(buildAgentRows([idle, working], locate, () => true, "priority"))).toBeUndefined();
  });

  it("counts only the rows waiting on a human as unread", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "priority");
    expect(unreadCount(rows)).toBe(2);
    expect(needsAttention("working")).toBe(false);
    expect(needsAttention("done")).toBe(true);
    expect(needsAttention("blocked")).toBe(true);
  });

  it("toggles between exactly two orderings", () => {
    expect(nextSortMode("grouped")).toBe("priority");
    expect(nextSortMode("priority")).toBe("grouped");
  });

  it("keeps agents whose workspace is not in the list last instead of dropping them", () => {
    const orphan = agent({ id: "orphan", sessionId: "$9", sessionName: "gone", displayName: "zed" });
    const rows = buildAgentRows([orphan, working], locate, () => false, "grouped");
    expect(rows.map((row) => row.agent.id)).toEqual(["working", "orphan"]);
    expect(rows.every((row) => row.routable)).toBe(false);
  });
});
