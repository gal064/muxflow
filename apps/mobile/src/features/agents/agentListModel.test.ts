import { describe, expect, it } from "vitest";

import { RECENT_WINDOW_MS } from "../../store/selectors";
import type { Agent, Session, SessionState, Window } from "../../store/sessionStore";
import {
  AGENT_LIST_MODES,
  buildAgentListItems,
  isAgentListMode,
  markState,
  nextRecentExpiration,
  priorityBucket,
  PRIORITY_SECTIONS,
  type AgentListItem,
} from "./agentListModel";

const NOW = 1_000_000_000;

type AgentOverrides = Partial<Agent> & { sessionId?: string; windowId?: string };

function agent(overrides: AgentOverrides & { id: string }): Agent {
  const { sessionId = "$1", windowId = "@1", ...rest } = overrides;
  return {
    adapterId: "codex",
    displayName: overrides.id,
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: NOW - 10_000,
    lifecycleChangedAtMs: NOW - 10_000,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId, sessionNameFallback: `fallback-${sessionId}`, windowId, windowNameFallback: `fallback-${windowId}`, paneId: "%1", paneIndexFallback: 0 },
    ...rest,
  };
}

const blocked = (id: string, extra: AgentOverrides = {}) =>
  agent({ id, lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 1n, ...extra });
const blockedSeen = (id: string, extra: AgentOverrides = {}) =>
  agent({ id, lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, seenGeneration: 2n, ...extra });
const working = (id: string, extra: AgentOverrides = {}) => agent({ id, lifecycle: "working", ...extra });
const done = (id: string, extra: AgentOverrides = {}) =>
  agent({ id, lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 1n, ...extra });
const freshIdle = (id: string, extra: AgentOverrides = {}) => agent({ id, lifecycle: "idle", lifecycleChangedAtMs: NOW - 1000, ...extra });
const oldIdle = (id: string, extra: AgentOverrides = {}) => agent({ id, lifecycle: "idle", lifecycleChangedAtMs: NOW - RECENT_WINDOW_MS - 1, ...extra });
const unknown = (id: string, extra: AgentOverrides = {}) => agent({ id, lifecycle: "unknown", ...extra });

function session(id: string, name: string, order: number, pinned = false): Session {
  return { id, name, windowCount: 1, order, pinned };
}
function window(id: string, sessionId: string, index: number, name: string, pinned = false): Window {
  return { id, sessionId, index, name, active: false, pinned };
}

function state(
  agents: Agent[],
  sessions: Session[] = [session("$1", "alpha", 0)],
  windows: Window[] = [window("@1", "$1", 0, "win-1")],
): Pick<SessionState, "agents" | "sessions" | "windows" | "adapters"> {
  return {
    agents: Object.fromEntries(agents.map((a) => [a.id, a])),
    sessions: Object.fromEntries(sessions.map((s) => [s.id, s])),
    windows: Object.fromEntries(windows.map((w) => [w.id, w])),
    adapters: [{ id: "codex", displayName: "Codex", hookWiring: "wired" }],
  };
}

/** Each row's pins by id: "", "window", "workspace" or "window+workspace". */
function pins(items: AgentListItem[]): Record<string, string> {
  return Object.fromEntries(items.filter((i) => i.kind === "agent").map((i) => [
    i.agent.id,
    [i.windowPinned ? "window" : "", i.workspacePinned ? "workspace" : ""].filter(Boolean).join("+"),
  ]));
}

/** A readable trace of the list: `#Label`, `>Workspace`, `--Divider`, `id`. */
function trace(items: AgentListItem[]): string[] {
  return items.map((item) => {
    switch (item.kind) {
      case "section": return `#${item.label}(${item.count})`;
      case "group": return `>${item.workspaceName}${item.pinned ? "*" : ""}(${item.count})`;
      case "divider": return `--${item.label}`;
      case "agent": return item.agent.id;
    }
  });
}

describe("mode", () => {
  it("names the two desktop modes and rejects anything else", () => {
    expect(AGENT_LIST_MODES.map((m) => m.mode)).toEqual(["priority", "workspace"]);
    expect(AGENT_LIST_MODES.map((m) => m.label)).toEqual(["Priority", "Workspace"]);
    expect(isAgentListMode("priority")).toBe(true);
    expect(isAgentListMode("workspace")).toBe(true);
    for (const bad of ["status", "grouped", "", undefined, null, 1, {}]) expect(isAgentListMode(bad)).toBe(false);
  });
});

describe("priorityBucket (desktop agentsList.ts rules)", () => {
  it.each<[string, Agent, string]>([
    ["blocked unread", blocked("a"), "blocked"],
    ["blocked seen", blockedSeen("a"), "blocked"],
    ["working", working("a"), "working"],
    ["done (unread completion) is recent", done("a"), "recent"],
    ["done stays recent however old the lifecycle change is", done("a", { lifecycleChangedAtMs: 0 }), "recent"],
    ["idle inside the window is recent", freshIdle("a"), "recent"],
    ["idle outside the window is idle", oldIdle("a"), "idle"],
    ["unknown shares idle", unknown("a"), "idle"],
    ["unknown changed just now still shares idle", unknown("a", { lifecycleChangedAtMs: NOW }), "idle"],
    ["gone blocked is idle", blocked("a", { present: false }), "idle"],
    ["gone working is idle", working("a", { present: false }), "idle"],
    ["gone done is idle", done("a", { present: false }), "idle"],
  ])("%s", (_name, a, bucket) => {
    expect(priorityBucket(a, NOW)).toBe(bucket);
  });

  it("uses the Recent window edges exactly: recent until now reaches the expiration", () => {
    const changedAt = NOW - RECENT_WINDOW_MS;
    expect(priorityBucket(agent({ id: "a", lifecycle: "idle", lifecycleChangedAtMs: changedAt + 1 }), NOW)).toBe("recent");
    expect(priorityBucket(agent({ id: "a", lifecycle: "idle", lifecycleChangedAtMs: changedAt }), NOW)).toBe("idle");
  });

  it("clocks a seen completion from the acknowledgement, not the lifecycle change", () => {
    const seenDone = agent({
      id: "a", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 2n,
      lifecycleChangedAtMs: NOW - RECENT_WINDOW_MS - 1, attentionSeenAtMs: NOW - 1000,
    });
    expect(priorityBucket(seenDone, NOW)).toBe("recent");
    expect(priorityBucket({ ...seenDone, attentionSeenAtMs: NOW - RECENT_WINDOW_MS }, NOW)).toBe("idle");
    // Without an acknowledgement clock the lifecycle change is the clock.
    expect(priorityBucket({ ...seenDone, attentionSeenAtMs: 0 }, NOW)).toBe("idle");
  });
});

describe("markState (the badge docked to the icon)", () => {
  it.each<[string, Agent, string]>([
    ["blocked unread → red dot", blocked("a"), "blocked"],
    ["blocked seen → still the red dot", blockedSeen("a"), "blocked"],
    ["working → spinner", working("a"), "working"],
    ["unread completion → done badge", done("a"), "done"],
    ["seen completion → nothing", agent({ id: "a", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 2n }), "idle"],
    ["fresh idle → nothing", freshIdle("a"), "idle"],
    ["unknown → dashed outline", unknown("a"), "unknown"],
    ["gone, whatever it was → nothing", blocked("a", { present: false }), "idle"],
  ])("%s", (_name, a, expected) => {
    expect(markState(a)).toBe(expected);
  });
});

describe("nextRecentExpiration (useRecentIdleClock target)", () => {
  it("is undefined with no idle rows or only expired ones", () => {
    expect(nextRecentExpiration([], NOW)).toBeUndefined();
    expect(nextRecentExpiration([working("a"), blocked("b"), done("c"), unknown("d"), oldIdle("e")], NOW)).toBeUndefined();
  });
  it("is the earliest future expiration among present idle rows", () => {
    const soon = freshIdle("soon", { lifecycleChangedAtMs: NOW - RECENT_WINDOW_MS + 500 });
    const later = freshIdle("later");
    const goneSooner = freshIdle("gone", { lifecycleChangedAtMs: NOW - RECENT_WINDOW_MS + 100, present: false });
    expect(nextRecentExpiration([later, soon, goneSooner], NOW)).toBe(NOW + 500);
  });
  it("treats an expiration exactly at now as past", () => {
    expect(nextRecentExpiration([freshIdle("a", { lifecycleChangedAtMs: NOW - RECENT_WINDOW_MS })], NOW)).toBeUndefined();
  });
});

describe("priority mode", () => {
  it("draws the four headings in order with their counts, and rows in sortedAgents order", () => {
    const items = buildAgentListItems(state([
      unknown("unknown"), oldIdle("old-idle"), freshIdle("fresh-idle"), done("done"),
      working("working"), blockedSeen("blocked-seen"), blocked("blocked-new"),
    ]), "priority", NOW);
    expect(trace(items)).toEqual([
      "#Blocked(2)", "blocked-new", "blocked-seen",
      "#Working(1)", "working",
      "#Recent(2)", "fresh-idle", "done",
      "#Idle(2)", "old-idle", "unknown",
    ]);
    expect(PRIORITY_SECTIONS.map((s) => s.state)).toEqual(["blocked", "working", "idle", "idle"]);
    expect(items.filter((i) => i.kind === "section").map((i) => (i as { state: string }).state)).toEqual(["blocked", "working", "idle", "idle"]);
  });

  it("omits empty sections", () => {
    expect(trace(buildAgentListItems(state([working("w")]), "priority", NOW))).toEqual(["#Working(1)", "w"]);
    expect(trace(buildAgentListItems(state([oldIdle("i"), blocked("b")]), "priority", NOW))).toEqual(["#Blocked(1)", "b", "#Idle(1)", "i"]);
    expect(buildAgentListItems(state([]), "priority", NOW)).toEqual([]);
  });

  it("puts gone agents at the bottom of Idle regardless of their retained lifecycle", () => {
    expect(trace(buildAgentListItems(state([
      blocked("gone-blocked", { present: false }), oldIdle("idle"), unknown("unknown"), blocked("live-blocked"),
    ]), "priority", NOW))).toEqual(["#Blocked(1)", "live-blocked", "#Idle(3)", "idle", "unknown", "gone-blocked"]);
  });

  it("leads with pinned rows inside a heading and marks them, without a pin outranking a louder heading", () => {
    const sessions = [session("$pinned", "pinned-ws", 1, true), session("$plain", "plain-ws", 0)];
    const windows = [window("@p", "$pinned", 0, "w"), window("@u", "$plain", 0, "w"), window("@tab", "$plain", 1, "w", true)];
    const items = buildAgentListItems(state([
      working("unpinned-newer", { sessionId: "$plain", windowId: "@u", lifecycleChangedAtMs: NOW - 1 }),
      working("ws-pinned", { sessionId: "$pinned", windowId: "@p", lifecycleChangedAtMs: NOW - 100 }),
      working("tab-pinned", { sessionId: "$plain", windowId: "@tab", lifecycleChangedAtMs: NOW - 50 }),
      oldIdle("pinned-idle", { sessionId: "$pinned", windowId: "@p" }),
    ], sessions, windows), "priority", NOW);
    expect(trace(items)).toEqual(["#Working(3)", "tab-pinned", "ws-pinned", "unpinned-newer", "#Idle(1)", "pinned-idle"]);
    expect(pins(items)).toEqual({
      "tab-pinned": "window",
      "ws-pinned": "workspace",
      "unpinned-newer": "",
      "pinned-idle": "workspace",
    });
  });

  it("marks a pinned window inside a pinned workspace with both pins, one per kind", () => {
    const sessions = [session("$pinned", "pinned-ws", 0, true)];
    const windows = [window("@tab", "$pinned", 0, "w", true), window("@plain", "$pinned", 1, "w")];
    const items = buildAgentListItems(state([
      working("both", { sessionId: "$pinned", windowId: "@tab" }),
      working("ws-only", { sessionId: "$pinned", windowId: "@plain" }),
    ], sessions, windows), "priority", NOW);
    expect(pins(items)).toEqual({ both: "window+workspace", "ws-only": "workspace" });
  });

  it("fills the row copy: title, workspace · window subtitle, attention, state", () => {
    const items = buildAgentListItems(state([blocked("b"), agent({ id: "w", displayName: "" })]), "priority", NOW);
    const rows = items.filter((i) => i.kind === "agent") as Extract<AgentListItem, { kind: "agent" }>[];
    expect(rows.map((r) => [r.title, r.subtitle, r.waiting, r.state])).toEqual([
      ["b", "alpha · win-1", "blocked", "blocked"],
      ["win-1", "alpha · win-1", undefined, "working"],
    ]);
  });

  it("keeps a seen blocked row waiting, clears a seen completion, and never a gone row (desktop needsAttention(state))", () => {
    const items = buildAgentListItems(state([
      blockedSeen("seen-blocked"),
      done("unread-done"),
      agent({ id: "seen-done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, seenGeneration: 2n, attentionSeenAtMs: NOW - 1 }),
      blocked("gone-blocked", { present: false }),
    ]), "priority", NOW);
    const waiting = Object.fromEntries(items.filter((i) => i.kind === "agent").map((i) => [i.agent.id, i.waiting]));
    expect(waiting).toEqual({ "seen-blocked": "blocked", "unread-done": "done", "seen-done": undefined, "gone-blocked": undefined });
  });

  it("gives every item a unique, stable key, unchanged when a row changes bucket", () => {
    const before = buildAgentListItems(state([working("a"), blocked("b")]), "priority", NOW);
    const after = buildAgentListItems(state([oldIdle("a"), blocked("b")]), "priority", NOW);
    const keys = (items: AgentListItem[]) => items.map((i) => i.key);
    expect(new Set(keys(before)).size).toBe(before.length);
    expect(new Set(keys(after)).size).toBe(after.length);
    const rowKey = (items: AgentListItem[], id: string) => items.find((i) => i.kind === "agent" && i.agent.id === id)?.key;
    expect(rowKey(before, "a")).toBe(rowKey(after, "a"));
    expect(rowKey(before, "b")).toBe(rowKey(after, "b"));
    expect(keys(before)).not.toEqual(keys(after));
  });

  it("moves a row from Recent to Idle once now passes its expiration, keeping the same key", () => {
    const s = state([freshIdle("a"), working("w")]);
    expect(trace(buildAgentListItems(s, "priority", NOW))).toEqual(["#Working(1)", "w", "#Recent(1)", "a"]);
    expect(trace(buildAgentListItems(s, "priority", NOW + RECENT_WINDOW_MS))).toEqual(["#Working(1)", "w", "#Idle(1)", "a"]);
  });
});

describe("workspace mode", () => {
  const sessions = [
    session("$c", "charlie", 2),
    session("$b", "bravo", 1, true),
    session("$a", "alpha", 0),
  ];
  const windows = [
    window("@a0", "$a", 0, "a-first"),
    window("@a1", "$a", 1, "a-second"),
    window("@b0", "$b", 0, "b-only"),
    window("@c0", "$c", 0, "c-first"),
    window("@c1", "$c", 1, "c-pinned", true),
  ];

  it("groups per workspace, pinned block above the Others divider, host order inside each block", () => {
    const items = buildAgentListItems(state([
      working("c-1", { sessionId: "$c", windowId: "@c0" }),
      blocked("a-2", { sessionId: "$a", windowId: "@a1" }),
      working("b-1", { sessionId: "$b", windowId: "@b0" }),
      oldIdle("a-1", { sessionId: "$a", windowId: "@a0" }),
    ], sessions, windows), "workspace", NOW);
    expect(trace(items)).toEqual([
      "--Pinned", ">bravo*(1)", "b-1",
      "--Others", ">alpha(2)", "a-1", "a-2", ">charlie(1)", "c-1",
    ]);
  });

  it("draws no divider when nothing is pinned", () => {
    const plain = sessions.map((s) => ({ ...s, pinned: false }));
    expect(trace(buildAgentListItems(state([
      working("c-1", { sessionId: "$c", windowId: "@c0" }), working("a-1", { sessionId: "$a", windowId: "@a0" }),
    ], plain, windows), "workspace", NOW))).toEqual([">alpha(1)", "a-1", ">charlie(1)", "c-1"]);
  });

  it("draws only the Pinned divider when every workspace with agents is pinned", () => {
    expect(trace(buildAgentListItems(state([
      working("b-1", { sessionId: "$b", windowId: "@b0" }),
    ], sessions, windows), "workspace", NOW))).toEqual(["--Pinned", ">bravo*(1)", "b-1"]);
  });

  it("orders rows inside a workspace: present, pinned window, window index, name, id", () => {
    const items = buildAgentListItems(state([
      working("c0-z", { sessionId: "$c", windowId: "@c0", displayName: "zed" }),
      working("c0-a", { sessionId: "$c", windowId: "@c0", displayName: "amy" }),
      working("c1", { sessionId: "$c", windowId: "@c1" }),
      working("c0-gone", { sessionId: "$c", windowId: "@c0", displayName: "aaa", present: false }),
      working("c0-a-twin", { sessionId: "$c", windowId: "@c0", displayName: "amy" }),
    ], sessions, windows), "workspace", NOW);
    // No pinned workspace has agents, so no divider is drawn.
    expect(trace(items)).toEqual([">charlie(5)", "c1", "c0-a", "c0-a-twin", "c0-z", "c0-gone"]);
    const rows = items.filter((i) => i.kind === "agent") as Extract<AgentListItem, { kind: "agent" }>[];
    // Only the pinned window earns a pin in this mode; the workspace pin is the divider.
    expect(rows.map((r) => [r.agent.id, r.windowPinned, r.subtitle])).toEqual([
      ["c1", true, "c-pinned"], ["c0-a", false, "c-first"], ["c0-a-twin", false, "c-first"], ["c0-z", false, "c-first"], ["c0-gone", false, "c-first"],
    ]);
  });

  it("leaves the workspace pin to the group heading: rows carry only their window's pin", () => {
    const items = buildAgentListItems(state([
      working("b-1", { sessionId: "$b", windowId: "@b0" }),
      working("c-1", { sessionId: "$c", windowId: "@c1" }),
    ], sessions, windows), "workspace", NOW);
    expect((items[1] as { pinned: boolean }).pinned).toBe(true);
    expect(pins(items)).toEqual({ "b-1": "", "c-1": "window" });
  });

  it("keeps a workspace the host no longer lists, named by the fallback, after the live ones", () => {
    const items = buildAgentListItems(state([
      working("ghost", { sessionId: "$gone", windowId: "@gone" }),
      working("a-1", { sessionId: "$a", windowId: "@a0" }),
    ], sessions, windows), "workspace", NOW);
    expect(trace(items)).toEqual([">alpha(1)", "a-1", ">fallback-$gone(1)", "ghost"]);
    expect((items.at(-1) as { subtitle: string }).subtitle).toBe("fallback-@gone");
  });

  it("keeps two same-order workspaces contiguous", () => {
    const tied = [session("$x", "same", 0), session("$y", "same", 0)];
    const wins = [window("@x", "$x", 0, "w"), window("@y", "$y", 0, "w")];
    const items = buildAgentListItems(state([
      working("y-1", { sessionId: "$y", windowId: "@y" }),
      working("x-1", { sessionId: "$x", windowId: "@x" }),
      working("y-2", { sessionId: "$y", windowId: "@y" }),
      working("x-2", { sessionId: "$x", windowId: "@x" }),
    ], tied, wins), "workspace", NOW);
    expect(trace(items)).toEqual([">same(2)", "x-1", "x-2", ">same(2)", "y-1", "y-2"]);
    expect(new Set(items.map((i) => i.key)).size).toBe(items.length);
  });

  it("is empty with no agents and uses the same row keys as priority mode", () => {
    expect(buildAgentListItems(state([]), "workspace", NOW)).toEqual([]);
    const s = state([working("a"), blocked("b")]);
    const key = (items: AgentListItem[], id: string) => items.find((i) => i.kind === "agent" && i.agent.id === id)?.key;
    const p = buildAgentListItems(s, "priority", NOW);
    const w = buildAgentListItems(s, "workspace", NOW);
    expect(key(p, "a")).toBe(key(w, "a"));
    expect(key(p, "b")).toBe(key(w, "b"));
  });
});
