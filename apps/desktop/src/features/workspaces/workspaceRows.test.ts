import { describe, expect, it } from "vitest";
import { agent } from "../agents/testFixtures";
import { compareAgents, deriveAgentRollups } from "../agents/selectors";
import type { TmuxSnapshot } from "../../app/types";
import type { AgentAdapterDescriptor } from "../agents/types";
import {
  WORKSPACE_ROW_AGENT_LIMIT, abbreviateHome, inferHome, sessionPath, workspaceMetaLine, workspaceRows,
} from "./workspaceRows";

const snapshot: TmuxSnapshot = {
  sessions: [
    { id: "$2", name: "project-e2e", windowCount: 1, attachedClients: 0, order: 1 },
    { id: "$1", name: "muxflow", windowCount: 2, attachedClients: 1, order: 0 },
  ],
  windows: [
    { id: "@1", sessionId: "$1", index: 1, name: "claude", active: false, layout: "" },
    { id: "@2", sessionId: "$1", index: 2, name: "zsh", active: true, layout: "" },
    { id: "@5", sessionId: "$2", index: 1, name: "shell", active: true, layout: "" },
  ],
  panes: [
    { id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/operator/dev/muxflow", currentCommand: "claude" },
    { id: "%2", sessionId: "$1", windowId: "@2", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/operator/dev/muxflow", currentCommand: "zsh" },
    { id: "%9", sessionId: "$2", windowId: "@5", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/operator/dev/project-e2e", currentCommand: "zsh" },
  ],
};

const agents = [
  agent({ id: "a", sessionId: "$1", windowId: "@1", paneId: "%1", displayName: "claude", lifecycle: "working", updatedAt: 20 }),
  agent({ id: "b", sessionId: "$1", windowId: "@2", paneId: "%2", displayName: "codex", lifecycle: "blocked", updatedAt: 10 }),
  agent({ id: "c", sessionId: "$2", windowId: "@5", paneId: "%9", displayName: "claude two", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4, seenGeneration: 1, updatedAt: 5 }),
];

const rows = () => workspaceRows({
  snapshot,
  activeSessionId: "$1",
  agents,
  attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
  activeBranch: "main*",
  home: "/home/operator",
});

describe("workspace sidebar rows", () => {
  it("orders by the workspace order tmux reports, not by discovery order", () => {
    expect(rows().map((row) => row.session.name)).toEqual(["muxflow", "project-e2e"]);
  });

  it("inherits the loudest agent's state and lists its agents loudest first", () => {
    const [primary, project] = rows();
    // blocked outranks working, so the workspace reads blocked even though a
    // working agent updated more recently — and leads the row's own list.
    expect(primary.attention).toBe("blocked");
    expect(primary.agents).toEqual([
      { id: "b", adapterId: "codex", name: "codex", state: "blocked" },
      { id: "a", adapterId: "codex", name: "claude", state: "working" },
    ]);
    expect(primary.agentOverflow).toBe(0);
    expect(project.agents).toEqual([{ id: "c", adapterId: "codex", name: "claude two", state: "done" }]);
  });

  it("uses useful live tab labels and hides machine identifiers in workspace summaries", () => {
    const labeled = [
      agent({ id: "named", sessionId: "$1", windowName: "Review auth flow", displayName: "Codex" }),
      agent({ id: "uuid", adapterId: "future", sessionId: "$2", windowName: "00000000-0000-0000-0000-000000000000", displayName: "00000000-0000-0000-0000-000000000000" }),
    ];
    const adapters = [{ id: "future", displayName: "Future agent" }] as AgentAdapterDescriptor[];
    const result = workspaceRows({ snapshot, agents: labeled, adapters, attentionByWorkspace: deriveAgentRollups(labeled).byWorkspace });
    expect(result[0].agents[0].name).toBe("Review auth flow");
    expect(result[0].agents[0].adapterId).toBe("codex");
    expect(result[1].agents[0].name).toBe("Future agent");
    expect(result[1].agents[0].adapterId).toBe("future");
    expect(JSON.stringify(result)).not.toContain("00000000-0000-0000-0000-000000000000");
  });

  it("lists three agents and counts the rest", () => {
    const many = [
      agent({ id: "n1", sessionId: "$1", displayName: "one", lifecycle: "idle", updatedAt: 1 }),
      agent({ id: "n2", sessionId: "$1", displayName: "two", lifecycle: "working", updatedAt: 2 }),
      agent({ id: "n3", sessionId: "$1", displayName: "three", lifecycle: "blocked", updatedAt: 3 }),
      agent({ id: "n4", sessionId: "$1", displayName: "four", lifecycle: "working", updatedAt: 9 }),
      agent({ id: "n5", sessionId: "$1", displayName: "five", lifecycle: "idle", updatedAt: 4 }),
    ];
    const [primary, project] = workspaceRows({
      snapshot, agents: many, attentionByWorkspace: deriveAgentRollups(many).byWorkspace,
    });
    expect(primary.agents.map((row) => row.name)).toEqual(["three", "four", "two"]);
    expect(primary.agents).toHaveLength(WORKSPACE_ROW_AGENT_LIMIT);
    expect(primary.agentOverflow).toBe(2);
    // A workspace with no agents counts nothing and lists nothing; the row must
    // not claim an overflow it does not have.
    expect(project.agents).toEqual([]);
    expect(project.agentOverflow).toBe(0);
  });

  it("ranks a row's agents exactly the way the agents list below it does", () => {
    // Two copies of "loudest" would put the same two agents in one order on
    // the workspace row and another in the list directly beneath it.
    const tied = [
      agent({ id: "z", sessionId: "$1", displayName: "zed", lifecycle: "working", updatedAt: 7 }),
      agent({ id: "a", sessionId: "$1", displayName: "ada", lifecycle: "working", updatedAt: 7 }),
      agent({ id: "m", sessionId: "$1", displayName: "mia", lifecycle: "working", updatedAt: 8 }),
    ];
    const rowOrder = (agents: typeof tied) => workspaceRows({
      snapshot, agents, attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
    })[0].agents.map((row) => row.id);
    expect(rowOrder(tied)).toEqual([...tied].sort(compareAgents).map((item) => item.id));
    // Recency, then name: "ada" before "zed" at the same update time.
    expect(rowOrder(tied)).toEqual(["m", "a", "z"]);
    // And the input's own order never leaks into the answer.
    expect(rowOrder([...tied].reverse())).toEqual(rowOrder(tied));
  });

  it("counts only agents waiting on a human as that workspace's unread badge", () => {
    const [primary, project] = rows();
    expect(primary.unread).toBe(1);
    expect(project.unread).toBe(1);
    // A workspace whose only agent is running has nothing to badge.
    const working = [agent({ id: "w", sessionId: "$1", lifecycle: "working" })];
    expect(workspaceRows({ snapshot, agents: working, attentionByWorkspace: deriveAgentRollups(working).byWorkspace })[0].unread).toBe(0);
    expect(workspaceRows({ snapshot, agents: working, attentionByWorkspace: deriveAgentRollups(working).byWorkspace })[0].working).toBe(true);
  });

  it("keeps branch and path apart, so the sidebar can show one and ⌘P can match both", () => {
    const [primary, project] = rows();
    expect(primary.branch).toBe("main*");
    expect(primary.path).toBe("~/dev/muxflow");
    // Git only ever has a snapshot for the active workspace, so the others
    // carry a path and claim no branch rather than guessing one.
    expect(project.branch).toBeUndefined();
    expect(project.path).toBe("~/dev/project-e2e");
    // ⌘P wants them back together, and the separator lives here rather than in
    // the component that would otherwise rebuild it on every keystroke.
    expect(workspaceMetaLine(primary)).toBe("main* · ~/dev/muxflow");
    expect(workspaceMetaLine(project)).toBe("~/dev/project-e2e");
    expect(workspaceMetaLine({})).toBe("");
  });

  it("says nothing about activity for a workspace with no agents", () => {
    const [primary] = workspaceRows({ snapshot, agents: [], attentionByWorkspace: new Map() });
    expect(primary.agents).toEqual([]);
    expect(primary.agentOverflow).toBe(0);
    expect(primary.attention).toBe("none");
  });

  it("takes the path from the active pane of the active window", () => {
    expect(sessionPath(snapshot, "$1")).toBe("/home/operator/dev/muxflow");
    expect(sessionPath(snapshot, "$nope")).toBeUndefined();
  });

  it("shortens paths the way a shell prompt does, and only when it can", () => {
    expect(abbreviateHome("/home/operator/dev/x", "/home/operator")).toBe("~/dev/x");
    expect(abbreviateHome("/home/operator", "/home/operator")).toBe("~");
    // A prefix match that is not a path boundary is not a home directory.
    expect(abbreviateHome("/home/operator-extra/x", "/home/operator")).toBe("/home/operator-extra/x");
    expect(abbreviateHome("/srv/app", "/home/operator")).toBe("/srv/app");
    expect(abbreviateHome(undefined, "/home/operator")).toBeUndefined();
  });

  it("infers the tmux user's home from where the panes are, or says nothing", () => {
    expect(inferHome(["/home/operator/dev/a", "/home/operator/dev/b", "/srv/x"])).toBe("/home/operator");
    expect(inferHome(["/Users/operator/dev/a", "/Users/operator"])).toBe("/Users/operator");
    expect(inferHome(["/root/x"])).toBe("/root");
    // Nothing home-shaped means no guess at all, rather than a wrong `~`.
    expect(inferHome(["/srv/app", "/var/lib", undefined])).toBeUndefined();
    expect(inferHome([])).toBeUndefined();
    // A single deploy pane under another user's home must not outvote the
    // user's own; ties resolve deterministically instead of by iteration order.
    expect(inferHome(["/home/operator/a", "/home/operator/b", "/home/deploy/c"])).toBe("/home/operator");
    expect(inferHome(["/home/zed/a", "/home/operator/b"])).toBe("/home/operator");
  });

  it("lists only pinned workspaces under the filter, and keeps the selected one either way", () => {
    const filtered = (pinnedAt: ReadonlyMap<string, number>, activeSessionId?: string) => workspaceRows({
      snapshot, activeSessionId, agents, attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
      pinnedOnly: true, pinnedAt,
    }).map((row) => row.session.id);
    // The selected workspace keeps its row even unpinned: the filter must never
    // hide what the shell is currently showing.
    expect(filtered(new Map([["$2", 1]]), "$1")).toEqual(["$2", "$1"]);
    expect(filtered(new Map([["$2", 1]]), "$2")).toEqual(["$2"]);
    // Nothing pinned and nothing selected is an empty list, not a full one.
    expect(filtered(new Map())).toEqual([]);
    // Off, the same inputs list everything.
    expect(workspaceRows({
      snapshot, activeSessionId: "$1", agents, attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
      pinnedAt: new Map([["$2", 1]]),
    })).toHaveLength(2);
  });
});

describe("pinned workspaces lead the sidebar list", () => {
  const pinnedRows = (pinnedAt: ReadonlyMap<string, number>) => workspaceRows({
    snapshot,
    activeSessionId: "$1",
    agents,
    attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
    pinnedAt,
  });

  it("moves a pinned workspace to the top and marks the row", () => {
    // `$2` sorts second by tmux order; pinning it puts it first.
    const list = pinnedRows(new Map([["$2", 10]]));
    expect(list.map((row) => row.session.id)).toEqual(["$2", "$1"]);
    expect(list.map((row) => row.pinned)).toEqual([true, false]);
  });

  it("keeps the first workspace pinned first, and the rest in tmux order", () => {
    const list = pinnedRows(new Map([["$1", 20], ["$2", 10]]));
    expect(list.map((row) => row.session.id)).toEqual(["$2", "$1"]);
    // This list is also ⌘1–9 and the ⌘P switcher, so the numbers follow it.
    expect(list.map((row) => row.session.name)).toEqual(["project-e2e", "muxflow"]);
  });

  it("changes nothing when no workspace on this server is pinned", () => {
    const list = pinnedRows(new Map([["$99", 10]]));
    expect(list.map((row) => row.session.id)).toEqual(["$1", "$2"]);
    expect(list.every((row) => !row.pinned)).toBe(true);
    expect(rows().map((row) => row.pinned)).toEqual([false, false]);
  });
});
