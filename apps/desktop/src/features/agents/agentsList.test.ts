import { describe, expect, it } from "vitest";
import {
  buildAgentRows, groupAgentRows, groupAgentRowsByStatus, jumpTarget, needsAttention, nextSortMode,
  RECENT_IDLE_WINDOW_MILLIS, sortModeLabel, unreadCount, type AgentLocation,
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

  it("splits Recent from Idle at six hours and keeps both newest-change first", () => {
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
 * Two facts reach a row: whether its workspace is pinned and whether its tab
 * is. The workspace ordering inherits the first through `workspaceOrder`,
 * because the sidebar list it is built from is already pinned-first; the
 * priority ordering has no workspace ranking to inherit and so reads the pins
 * itself. Both modes are asserted here, because the whole point of the
 * feature is that they agree about what comes first.
 */
describe("pinned agents lead the list in both orderings", () => {
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
  // The quiet agent in the pinned workspace and the loud one in the plain
  // workspace: without the pin, `blockedElsewhere` sorts first in both modes.
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

  it("puts a pinned workspace's agents first in the priority ordering", () => {
    const rows = buildAgentRows(all, place, () => true, "status");
    // Blocked would otherwise be first; the pin outranks the status ranking,
    // and lifecycle recency — not the source of the pin — orders equal states.
    expect(rows.map((row) => row.agent.id)).toEqual(["idle-here", "pinned-tab", "blocked-away"]);
    expect(rows.map((row) => row.pinned)).toEqual([true, true, false]);
  });

  it("puts a pinned workspace's agents first in the workspace ordering too", () => {
    const rows = buildAgentRows(all, place, () => true, "workspace");
    // Inside the workspace the pinned tab leads despite its higher tab index —
    // the strip's own order, which is what this mode follows.
    expect(rows.map((row) => row.agent.id)).toEqual(["pinned-tab", "idle-here", "blocked-away"]);
  });

  it("lifts the pinned block above the status headings rather than into them", () => {
    const groups = groupAgentRowsByStatus(buildAgentRows(all, place, () => true, "status"));
    expect(groups.map((group) => [group.label, group.rows.map((row) => row.agent.id)])).toEqual([
      ["Pinned", ["idle-here", "pinned-tab"]],
      ["Blocked", ["blocked-away"]],
    ]);
    // A pinned row inside an Idle bucket would be first in its group and fourth
    // on screen, which is not what a pin promises. The block has no state dot
    // for the same reason: "pinned" is not something an agent is doing.
    expect(groups[0].state).toBeUndefined();
    expect(groups.flatMap((group) => group.rows)).toHaveLength(3);
  });

  it("orders one unified pinned block by status, regardless of the pin source", () => {
    const strayPinnedTab = agent({ id: "stray", displayName: "d", sessionId: "$plain", windowId: "@pinned", lifecycle: "blocked", updatedAt: 50 });
    const rows = buildAgentRows([...all, strayPinnedTab], place, () => true, "status");
    expect(rows.map((row) => row.agent.id)).toEqual(["stray", "idle-here", "pinned-tab", "blocked-away"]);
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
      ? { workspaceOrder: 0, workspaceName: "pinned-ws", workspacePinned: true }
      : { workspaceOrder: 1, workspaceName: "plain-ws" };
    const rows = buildAgentRows([pinnedDone, blockedElsewhere], place, () => true, "status");
    // The list leads with the pinned row…
    expect(rows[0].agent.id).toBe("pinned-done");
    // …and the bell still means "who needs me most".
    expect(jumpTarget(rows)?.agent.id).toBe("blocked-away");
  });
});
