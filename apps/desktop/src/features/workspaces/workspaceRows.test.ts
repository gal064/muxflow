import { describe, expect, it } from "vitest";
import { agent } from "../agents/testFixtures";
import { compareAgents, deriveAgentRollups } from "../agents/selectors";
import type { TmuxSnapshot } from "../../app/types";
import {
  WORKSPACE_ROW_AGENT_LIMIT, abbreviateHome, inferHome, sessionPath, workspaceMetaLine, workspaceRows,
} from "./workspaceRows";

const snapshot: TmuxSnapshot = {
  sessions: [
    { id: "$2", name: "sampleco-e2e", windowCount: 1, attachedClients: 0, order: 1 },
    { id: "$1", name: "muxflow", windowCount: 2, attachedClients: 1, order: 0 },
  ],
  windows: [
    { id: "@1", sessionId: "$1", index: 1, name: "claude", active: false, layout: "" },
    { id: "@2", sessionId: "$1", index: 2, name: "zsh", active: true, layout: "" },
    { id: "@5", sessionId: "$2", index: 1, name: "shell", active: true, layout: "" },
  ],
  panes: [
    { id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/user/dev/muxflow", currentCommand: "claude" },
    { id: "%2", sessionId: "$1", windowId: "@2", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/user/dev/muxflow", currentCommand: "zsh" },
    { id: "%9", sessionId: "$2", windowId: "@5", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/user/dev/checksum", currentCommand: "zsh" },
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
  home: "/home/user",
});

describe("workspace sidebar rows", () => {
  it("orders by the workspace order tmux reports, not by discovery order", () => {
    expect(rows().map((row) => row.session.name)).toEqual(["muxflow", "sampleco-e2e"]);
  });

  it("inherits the loudest agent's state and lists its agents loudest first", () => {
    const [galAde, checksum] = rows();
    // blocked outranks working, so the workspace reads blocked even though a
    // working agent updated more recently — and leads the row's own list.
    expect(galAde.attention).toBe("blocked");
    expect(galAde.agents).toEqual([
      { id: "b", name: "codex", state: "blocked" },
      { id: "a", name: "claude", state: "working" },
    ]);
    expect(galAde.agentOverflow).toBe(0);
    expect(sampleco.agents).toEqual([{ id: "c", name: "claude two", state: "done" }]);
  });

  it("lists three agents and counts the rest", () => {
    const many = [
      agent({ id: "n1", sessionId: "$1", displayName: "one", lifecycle: "idle", updatedAt: 1 }),
      agent({ id: "n2", sessionId: "$1", displayName: "two", lifecycle: "working", updatedAt: 2 }),
      agent({ id: "n3", sessionId: "$1", displayName: "three", lifecycle: "blocked", updatedAt: 3 }),
      agent({ id: "n4", sessionId: "$1", displayName: "four", lifecycle: "working", updatedAt: 9 }),
      agent({ id: "n5", sessionId: "$1", displayName: "five", lifecycle: "idle", updatedAt: 4 }),
    ];
    const [galAde, checksum] = workspaceRows({
      snapshot, agents: many, attentionByWorkspace: deriveAgentRollups(many).byWorkspace,
    });
    expect(galAde.agents.map((row) => row.name)).toEqual(["three", "four", "two"]);
    expect(galAde.agents).toHaveLength(WORKSPACE_ROW_AGENT_LIMIT);
    expect(galAde.agentOverflow).toBe(2);
    // A workspace with no agents counts nothing and lists nothing; the row must
    // not claim an overflow it does not have.
    expect(sampleco.agents).toEqual([]);
    expect(sampleco.agentOverflow).toBe(0);
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
    const [galAde, checksum] = rows();
    expect(galAde.unread).toBe(1);
    expect(sampleco.unread).toBe(1);
    // A workspace whose only agent is running has nothing to badge.
    const working = [agent({ id: "w", sessionId: "$1", lifecycle: "working" })];
    expect(workspaceRows({ snapshot, agents: working, attentionByWorkspace: deriveAgentRollups(working).byWorkspace })[0].unread).toBe(0);
    expect(workspaceRows({ snapshot, agents: working, attentionByWorkspace: deriveAgentRollups(working).byWorkspace })[0].working).toBe(true);
  });

  it("keeps branch and path apart, so the sidebar can show one and ⌘P can match both", () => {
    const [galAde, checksum] = rows();
    expect(galAde.branch).toBe("main*");
    expect(galAde.path).toBe("~/dev/muxflow");
    // Git only ever has a snapshot for the active workspace, so the others
    // carry a path and claim no branch rather than guessing one.
    expect(sampleco.branch).toBeUndefined();
    expect(sampleco.path).toBe("~/dev/checksum");
    // ⌘P wants them back together, and the separator lives here rather than in
    // the component that would otherwise rebuild it on every keystroke.
    expect(workspaceMetaLine(galAde)).toBe("main* · ~/dev/muxflow");
    expect(workspaceMetaLine(checksum)).toBe("~/dev/checksum");
    expect(workspaceMetaLine({})).toBe("");
  });

  it("says nothing about activity for a workspace with no agents", () => {
    const [galAde] = workspaceRows({ snapshot, agents: [], attentionByWorkspace: new Map() });
    expect(galAde.agents).toEqual([]);
    expect(galAde.agentOverflow).toBe(0);
    expect(galAde.attention).toBe("none");
  });

  it("takes the path from the active pane of the active window", () => {
    expect(sessionPath(snapshot, "$1")).toBe("/home/user/dev/muxflow");
    expect(sessionPath(snapshot, "$nope")).toBeUndefined();
  });

  it("shortens paths the way a shell prompt does, and only when it can", () => {
    expect(abbreviateHome("/home/user/dev/x", "/home/user")).toBe("~/dev/x");
    expect(abbreviateHome("/home/user", "/home/user")).toBe("~");
    // A prefix match that is not a path boundary is not a home directory.
    expect(abbreviateHome("/home/useraxy/x", "/home/user")).toBe("/home/useraxy/x");
    expect(abbreviateHome("/srv/app", "/home/user")).toBe("/srv/app");
    expect(abbreviateHome(undefined, "/home/user")).toBeUndefined();
  });

  it("infers the tmux user's home from where the panes are, or says nothing", () => {
    expect(inferHome(["/home/user/dev/a", "/home/user/dev/b", "/srv/x"])).toBe("/home/user");
    expect(inferHome(["/Users/user/dev/a", "/Users/user"])).toBe("/Users/user");
    expect(inferHome(["/root/x"])).toBe("/root");
    // Nothing home-shaped means no guess at all, rather than a wrong `~`.
    expect(inferHome(["/srv/app", "/var/lib", undefined])).toBeUndefined();
    expect(inferHome([])).toBeUndefined();
    // A single deploy pane under another user's home must not outvote the
    // user's own; ties resolve deterministically instead of by iteration order.
    expect(inferHome(["/home/user/a", "/home/user/b", "/home/deploy/c"])).toBe("/home/user");
    expect(inferHome(["/home/zed/a", "/home/user/b"])).toBe("/home/user");
  });
});
