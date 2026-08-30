import { describe, expect, it } from "vitest";
import {
  buildAgentRows, groupAgentRows, groupAgentRowsByStatus, jumpTarget, needsAttention, nextSortMode,
  RECENT_IDLE_WINDOW_MILLIS, selectedAgentIdForPane, sortModeLabel, unreadCount, type AgentLocation,
} from "./agentsList";
import { agent } from "./testFixtures";
import type { AgentRecord } from "./types";

const locations: Record<string, AgentLocation> = {
  "$1": { workspaceOrder: 0, workspaceName: "muxflow", tabIndex: 1 },
  "$2": { workspaceOrder: 1, workspaceName: "project-e2e", tabIndex: 2 },
};
const locate = (record: AgentRecord): AgentLocation =>
  locations[record.sessionId] ?? { workspaceOrder: Number.MAX_SAFE_INTEGER, workspaceName: record.sessionName };

const working = agent({ id: "working", displayName: "claude", lifecycle: "working", updatedAt: 30 });
const blocked = agent({ id: "blocked", displayName: "codex", lifecycle: "blocked", sessionId: "$1", updatedAt: 20 });
const done = agent({ id: "done", displayName: "claude two", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4, seenGeneration: 2, sessionId: "$2", updatedAt: 10 });
const idle = agent({ id: "idle", displayName: "claude three", lifecycle: "idle", sessionId: "$2", updatedAt: 5 });

describe("agents section ordering", () => {
  it("ranks the visual queue blocked > working > done > idle", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "status");
    expect(rows.map((row) => row.agent.id)).toEqual(["blocked", "working", "done", "idle"]);
  });

  it("follows the workspace list, then tab order, in workspace mode", () => {
    const rows = buildAgentRows([done, idle, blocked, working], locate, () => true, "workspace");
    expect(rows.map((row) => row.location.workspaceName)).toEqual(["muxflow", "muxflow", "project-e2e", "project-e2e"]);
    // Within a workspace, ties fall back to the agent's own name so the list
    // does not reshuffle on every update.
    expect(rows.slice(2).map((row) => row.agent.displayName)).toEqual(["claude three", "claude two"]);
  });

  it("sends ⌘⇧U to the loudest reachable agent that actually wants something", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "workspace");
    expect(jumpTarget(rows)?.agent.id).toBe("blocked");
    // Ordering mode must not change where the jump lands.
    expect(jumpTarget(buildAgentRows([idle, working, done, blocked], locate, () => true, "status"))?.agent.id).toBe("blocked");
    // An unroutable row is not a destination; the next one down is.
    expect(jumpTarget(rows.map((row) => row.agent.id === "blocked" ? { ...row, routable: false } : row))?.agent.id).toBe("done");
    // Nothing waiting means nothing to jump to, rather than "jump to whatever
    // sorted first".
    expect(jumpTarget(buildAgentRows([idle, working], locate, () => true, "status"))).toBeUndefined();
  });

  it("puts blocked ahead of done-unread whatever the timestamps say", () => {
    // Blocked is the older of the two here, so only the status ranking — not
    // the recency tiebreak — can put it first.
    const stale = agent({ id: "blocked", lifecycle: "blocked", sessionId: "$1", updatedAt: 1 });
    const fresh = agent({
      id: "done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4,
      seenGeneration: 2, sessionId: "$2", updatedAt: 99,
    });
    expect(jumpTarget(buildAgentRows([fresh, stale], locate, () => true, "status"))?.agent.id).toBe("blocked");
    expect(jumpTarget(buildAgentRows([fresh, stale], locate, () => true, "workspace"))?.agent.id).toBe("blocked");
  });

  it("counts only the rows waiting on a human as unread", () => {
    const rows = buildAgentRows([idle, working, done, blocked], locate, () => true, "status");
    expect(unreadCount(rows)).toBe(2);
    expect(needsAttention("working")).toBe(false);
    expect(needsAttention("done")).toBe(true);
    expect(needsAttention("blocked")).toBe(true);
  });

  it("toggles between exactly two orderings", () => {
    expect(nextSortMode("workspace")).toBe("status");
    expect(nextSortMode("status")).toBe("workspace");
    // The persisted value is `status` — it is in the app-state contract and two
    // migrations point at it — but the button says what the mode does.
    expect(sortModeLabel("status")).toBe("priority");
    expect(sortModeLabel("workspace")).toBe("workspace");
  });

  it("buckets the priority order into the four groups you read top to bottom", () => {
    const unknown = agent({ id: "unknown", lifecycle: "unknown" });
    const rows = buildAgentRows([idle, working, done, blocked, unknown], locate, () => true, "status");
    const groups = groupAgentRowsByStatus(rows);
    // Not the sort's order: `compareAgents` ranks done-unread above working,
    // which is right for "where does ⌘⇧U land" and wrong for a column read top
    // to bottom, where Working between Blocked and Done is what makes it a
    // queue. Unknown shares Idle's bucket — a gap in reporting is not a fifth
    // thing an agent can be doing.
    expect(groups.map((group) => [group.label, group.rows.map((row) => row.agent.id)])).toEqual([
      ["Blocked", ["blocked"]],
      ["Working", ["working"]],
      ["Done", ["done"]],
      ["Idle", ["idle", "unknown"]],
    ]);
    // Every group draws a state dot, and it is the group's state, not a row's.
    expect(groups.map((group) => group.state)).toEqual(["blocked", "working", "done", "idle"]);
    // An empty bucket is absent, not an empty heading.
    expect(groupAgentRowsByStatus(buildAgentRows([working], locate, () => true, "status"))
      .map((group) => group.key)).toEqual(["working"]);
    expect(groupAgentRowsByStatus([])).toEqual([]);
    // Rows keep their sorted order inside a bucket, and no row is lost.
    expect(groups.flatMap((group) => group.rows)).toHaveLength(rows.length);
  });

  it("orders every status by its last lifecycle change, not its latest hook update", () => {
    const first = agent({
      id: "first", displayName: "z", lifecycle: "working", updatedAt: 10,
      lifecycleChangedAt: 300,
    });
    const second = agent({
      id: "second", displayName: "a", lifecycle: "working", updatedAt: 999,
      lifecycleChangedAt: 200,
    });
    const order = (agents: AgentRecord[]) => buildAgentRows(
      agents, locate, () => true, "status", 1_000,
    ).map((row) => row.agent.id);
    expect(order([second, first])).toEqual(["first", "second"]);
    expect(order([{ ...first, updatedAt: 2_000 }, { ...second, updatedAt: 3_000 }]))
      .toEqual(["first", "second"]);
  });

  it("uses an immutable tie-break when lifecycle change times match", () => {
    const alpha = agent({
      id: "alpha", displayName: "z", sessionId: "$2", lifecycle: "working",
      lifecycleChangedAt: 300,
    });
    const omega = agent({
      id: "omega", displayName: "a", sessionId: "$1", lifecycle: "working",
      lifecycleChangedAt: 300,
    });
    const order = (agents: AgentRecord[]) => buildAgentRows(
      agents, locate, () => true, "status", 1_000,
    ).map((row) => row.agent.id);
    expect(order([omega, alpha])).toEqual(["alpha", "omega"]);
    expect(order([
      { ...omega, displayName: "zz", sessionId: "$2" },
      { ...alpha, displayName: "aa", sessionId: "$1" },
    ])).toEqual(["alpha", "omega"]);
  });

  it("splits Recent from Idle at four hours and keeps both newest-change first", () => {
    const now = 10 * RECENT_IDLE_WINDOW_MILLIS;
    const agents = [
      agent({ id: "old-idle", lifecycle: "idle", lifecycleChangedAt: now - 8 * 60 * 60 * 1_000 }),
      agent({ id: "new-recent", lifecycle: "idle", lifecycleChangedAt: now - 60 * 60 * 1_000 }),
      agent({ id: "new-idle", lifecycle: "idle", lifecycleChangedAt: now - 7 * 60 * 60 * 1_000 }),
      agent({ id: "old-recent", lifecycle: "idle", lifecycleChangedAt: now - 2 * 60 * 60 * 1_000 }),
      agent({ id: "boundary", lifecycle: "idle", lifecycleChangedAt: now - RECENT_IDLE_WINDOW_MILLIS }),
      agent({ id: "unknown", lifecycle: "unknown", lifecycleChangedAt: now - 1 }),
    ];
    const rows = buildAgentRows(agents, locate, () => true, "status", now);
    expect(groupAgentRowsByStatus(rows).map((group) => [
      group.label,
      group.rows.map((row) => row.agent.id),
    ])).toEqual([
      ["Recent", ["new-recent", "old-recent"]],
      ["Idle", ["unknown", "boundary", "new-idle", "old-idle"]],
    ]);
  });

  it("moves acknowledged Done into Recent without resetting its completion time", () => {
    const now = 100_000_000;
    const completion = now - 60 * 60 * 1_000;
    const completed = agent({
      id: "completed", lifecycle: "idle", attentionKind: "completed",
      attentionGeneration: 4, seenGeneration: 1, lifecycleChangedAt: completion,
    });
    const doneRow = buildAgentRows([completed], locate, () => true, "status", now)[0];
    const acknowledgedRow = buildAgentRows(
      [{ ...completed, seenGeneration: completed.attentionGeneration }],
      locate,
      () => true,
      "status",
      now,
    )[0];
    expect([doneRow.priorityBucket, acknowledgedRow.priorityBucket]).toEqual(["done", "recent"]);
    expect(acknowledgedRow.agent.lifecycleChangedAt).toBe(completion);
  });

  it("keeps agents whose workspace is not in the list last instead of dropping them", () => {
    const orphan = agent({ id: "orphan", sessionId: "$9", sessionName: "gone", displayName: "zed" });
    const rows = buildAgentRows([orphan, working], locate, () => false, "workspace");
    expect(rows.map((row) => row.agent.id)).toEqual(["working", "orphan"]);
    expect(rows.every((row) => row.routable)).toBe(false);
  });

  it("groups by host, server, and workspace without changing display order", () => {
    const first = agent({ id: "a", hostProfileId: "local", serverIdentity: "one", sessionId: "$1", sessionName: "api" });
    const second = agent({ id: "b", hostProfileId: "remote", serverIdentity: "two", sessionId: "$1", sessionName: "api" });
    const third = agent({ id: "c", hostProfileId: "local", serverIdentity: "one", sessionId: "$1", sessionName: "api" });
    const rows = buildAgentRows([first, third, second], (record) => ({
      workspaceOrder: record.hostProfileId === "local" ? 0 : 1,
      workspaceName: "api",
      hostLabel: record.hostProfileId === "local" ? "This Mac" : "build-box",
    }), () => true, "workspace");
    const groups = groupAgentRows(rows);
    expect(groups.map(({ workspaceName, hostLabel }) => [workspaceName, hostLabel])).toEqual([
      ["api", "This Mac"], ["api", "build-box"],
    ]);
    expect(groups.flatMap((group) => group.rows.map((row) => row.agent.id)))
      .toEqual(rows.map((row) => row.agent.id));
  });
});

/**
 * Pinning, from the agents list's side.
 *
 * Workspace pins order whole workspaces through `workspaceOrder`. A tab pin
 * orders that agent only inside its workspace in workspace mode, or inside its
 * status in priority mode.
 */
describe("agent pins stay local to their workspace or status", () => {
  // Two workspaces: `$pin` is pinned, `$plain` is not. In the workspace
  // ordering the sidebar has already put `$pin` first, which is what
  // `workspaceOrder` says.
  const located: Record<string, AgentLocation> = {
    "$pin": { workspaceOrder: 0, workspaceName: "pinned-ws", tabIndex: 3, workspacePinned: true },
    "$plain": { workspaceOrder: 1, workspaceName: "plain-ws", tabIndex: 1 },
  };
  const place = (record: AgentRecord): AgentLocation => {
    const base = located[record.sessionId] ?? { workspaceOrder: 2, workspaceName: record.sessionName };
    return record.windowId === "@pinned" ? { ...base, tabIndex: 9, tabPinned: true } : base;
  };
  // A pinned workspace still leads only through workspace order. The tab pin
  // is the per-agent pin drawn on the row.
  const idleHere = agent({
    id: "idle-here", displayName: "a", sessionId: "$pin", lifecycle: "idle",
    updatedAt: 1, lifecycleChangedAt: 200,
  });
  const idleHerePinnedTab = agent({
    id: "pinned-tab", displayName: "b", sessionId: "$pin", windowId: "@pinned",
    lifecycle: "idle", updatedAt: 2, lifecycleChangedAt: 100,
  });
  const blockedElsewhere = agent({ id: "blocked-away", displayName: "c", sessionId: "$plain", lifecycle: "blocked", updatedAt: 99 });
  const all = [blockedElsewhere, idleHere, idleHerePinnedTab];

  it("keeps status ahead of pins in the priority ordering", () => {
    const rows = buildAgentRows(all, place, () => true, "status");
    expect(rows.map((row) => row.agent.id)).toEqual(["blocked-away", "pinned-tab", "idle-here"]);
    expect(rows.map((row) => row.pinned)).toEqual([false, true, false]);
  });

  it("puts a pinned workspace's agents first in the workspace ordering too", () => {
    const rows = buildAgentRows(all, place, () => true, "workspace");
    // Inside the workspace the pinned tab leads despite its higher tab index —
    // the strip's own order, which is what this mode follows.
    expect(rows.map((row) => row.agent.id)).toEqual(["pinned-tab", "idle-here", "blocked-away"]);
  });

  it("keeps pinned agents in their status heading", () => {
    const groups = groupAgentRowsByStatus(buildAgentRows(all, place, () => true, "status"));
    expect(groups.map((group) => [group.label, group.rows.map((row) => row.agent.id)])).toEqual([
      ["Blocked", ["blocked-away"]],
      ["Idle", ["pinned-tab", "idle-here"]],
    ]);
    expect(groups.map((group) => group.state)).toEqual(["blocked", "idle"]);
    expect(groups.flatMap((group) => group.rows)).toHaveLength(3);
  });

  it("puts pinned agents before unpinned peers inside each status", () => {
    const strayPinnedTab = agent({ id: "stray", displayName: "d", sessionId: "$plain", windowId: "@pinned", lifecycle: "blocked", updatedAt: 50 });
    const rows = buildAgentRows([...all, strayPinnedTab], place, () => true, "status");
    expect(rows.map((row) => row.agent.id)).toEqual(["stray", "blocked-away", "pinned-tab", "idle-here"]);
  });

  it("orders lifecycle recency separately inside pinned and unpinned peers", () => {
    const rows = buildAgentRows([
      agent({ id: "pinned-old", lifecycle: "working", lifecycleChangedAt: 200, windowId: "@pinned" }),
      agent({ id: "loose-new", lifecycle: "working", lifecycleChangedAt: 400, windowId: "@loose-new" }),
      agent({ id: "pinned-new", lifecycle: "working", lifecycleChangedAt: 300, windowId: "@pinned" }),
      agent({ id: "loose-old", lifecycle: "working", lifecycleChangedAt: 100, windowId: "@loose-old" }),
    ], place, () => true, "status", 1_000);
    expect(rows.map((row) => row.agent.id)).toEqual([
      "pinned-new", "pinned-old", "loose-new", "loose-old",
    ]);
  });

  it("leaves both orderings exactly as they were when nothing is pinned", () => {
    const plain = (record: AgentRecord): AgentLocation => locations[record.sessionId]
      ?? { workspaceOrder: Number.MAX_SAFE_INTEGER, workspaceName: record.sessionName };
    expect(buildAgentRows([idle, working, done, blocked], plain, () => true, "status").map((row) => row.agent.id))
      .toEqual(["blocked", "working", "done", "idle"]);
    expect(buildAgentRows([idle, working, done, blocked], plain, () => true, "workspace").map((row) => row.pinned))
      .toEqual([false, false, false, false]);
  });
});

describe("the bell and ⌘⇧U ignore pins", () => {
  it("still goes to the loudest waiting agent, not to a pinned quieter one", () => {
    const pinnedDone = agent({
      id: "pinned-done", displayName: "a", sessionId: "$pin", lifecycle: "idle",
      attentionKind: "completed", attentionGeneration: 4, seenGeneration: 1, updatedAt: 1,
    });
    const blockedElsewhere = agent({ id: "blocked-away", displayName: "c", sessionId: "$plain", lifecycle: "blocked", updatedAt: 99 });
    const place = (record: AgentRecord): AgentLocation => record.sessionId === "$pin"
      ? { workspaceOrder: 0, workspaceName: "pinned-ws", tabPinned: true }
      : { workspaceOrder: 1, workspaceName: "plain-ws" };
    const rows = buildAgentRows([pinnedDone, blockedElsewhere], place, () => true, "status");
    expect(rows[0].agent.id).toBe("blocked-away");
    expect(jumpTarget(rows)?.agent.id).toBe("blocked-away");
  });
});

describe("active pane agent selection", () => {
  const rowsForSelection = (agents: readonly AgentRecord[], routable = () => true) =>
    buildAgentRows(agents, locate, routable, "status");

  it("selects the routable agent whose exact pane is active, including idle agents", () => {
    const activeIdle = agent({ id: "active-idle", paneId: "%7", lifecycle: "idle" });
    const elsewhere = agent({ id: "elsewhere", paneId: "%8", lifecycle: "working" });
    const rows = rowsForSelection([elsewhere, activeIdle]);
    expect(selectedAgentIdForPane(rows, "%7")).toBe("active-idle");
    expect(selectedAgentIdForPane(rows, "%missing")).toBeUndefined();
    expect(selectedAgentIdForPane(rows, undefined)).toBeUndefined();
    expect(selectedAgentIdForPane(rowsForSelection([activeIdle], () => false), "%7")).toBeUndefined();
  });

  it("resolves a defensive duplicate to native, then newest, then stable id", () => {
    const manual = agent({
      id: "manual", paneId: "%7", detectedManually: true, nativeSessionId: "", updatedAt: 999,
    });
    const nativeOld = agent({
      id: "native-old", paneId: "%7", detectedManually: false, nativeSessionId: "session-old", updatedAt: 10,
    });
    const nativeNewB = agent({
      id: "native-b", paneId: "%7", detectedManually: false, nativeSessionId: "session-b", updatedAt: 20,
    });
    const nativeNewA = agent({
      id: "native-a", paneId: "%7", detectedManually: false, nativeSessionId: "session-a", updatedAt: 20,
    });
    expect(selectedAgentIdForPane(rowsForSelection([manual, nativeOld]), "%7")).toBe("native-old");
    expect(selectedAgentIdForPane(rowsForSelection([nativeOld, nativeNewB]), "%7")).toBe("native-b");
    expect(selectedAgentIdForPane(rowsForSelection([nativeNewB, nativeNewA]), "%7")).toBe("native-a");
  });
});
