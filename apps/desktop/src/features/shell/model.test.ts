import { describe, expect, it } from "vitest";
import type { Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import {
  appTabsForWorkspace,
  archiveWorkspace,
  archivedSessionIds,
  archivedWorkspacesFor,
  bulkCloseOutcomeStatus,
  closeAppTab,
  closeTransientGitDiff,
  combineWorkspaceTabs,
  discardServerAppState,
  mountedAppTabIds,
  mountedTerminalPanes,
  openFileTab,
  findGitDiffTab,
  openGitDiffTab,
  orderedSessions,
  pinAppTab,
  relocateFileTabs,
  retirePendingTab,
  reconcileWorkspaceIdentity,
  recoverableAppTabCount,
  recoverAppTabsFromPreviousServer,
  reorderAppTab,
  resolveAgentShellDestination,
  resolveSelectedSession,
  selectAppTab,
  setMarkdownViewMode,
  shouldSurfaceAuthoritativeTerminal,
  shellNavigationMode,
  tabsToCloseOthers,
  tabsToCloseNonAgent,
  tabsToCloseRight,
  tabsEligibleAtBulkCloseCommit,
  unarchiveWorkspace,
  setWorkspaceDefaults,
  workspaceDefaultsFor,
  type CombinedTab,
} from "./model";
import { defaultAppState, type PersistedAppState } from "./types";
import type { GitStatusEntry, GitStatusSnapshot } from "../git/types";
import { deriveAgentRollups } from "../agents/selectors";
import { agent } from "../agents/testFixtures";

describe("per-host workspace defaults", () => {
  it("edits one host without touching another, and drops an entry with nothing left in it", () => {
    const local = setWorkspaceDefaults(defaultAppState, "local", { directory: "/work" });
    const both = setWorkspaceDefaults(local, "ssh-remote", { startupCommand: "tail -f log" });
    expect(workspaceDefaultsFor(both, "local")).toEqual({ directory: "/work" });
    expect(workspaceDefaultsFor(both, "ssh-remote")).toEqual({ startupCommand: "tail -f log" });
    // A host nobody configured gets nothing, which is what "existing users keep
    // the current behaviour" is made of.
    expect(workspaceDefaultsFor(both, "ssh-other")).toEqual({});

    const patched = setWorkspaceDefaults(both, "local", { startupCommand: "  npm run dev  " });
    expect(workspaceDefaultsFor(patched, "local")).toEqual({ directory: "/work", startupCommand: "npm run dev" });

    const cleared = setWorkspaceDefaults(
      setWorkspaceDefaults(patched, "local", { directory: undefined }),
      "local",
      { startupCommand: "   " },
    );
    expect(cleared.workspaceDefaults).toEqual({ "ssh-remote": { startupCommand: "tail -f log" } });
  });

  it("bounds what it writes, not only what it reads back", () => {
    const written = setWorkspaceDefaults(defaultAppState, "local", { startupCommand: "y".repeat(9_000) });
    expect(workspaceDefaultsFor(written, "local").startupCommand).toHaveLength(2_048);
  });
});

const sessions: Session[] = [
  { id: "$2", name: "two", windowCount: 1, attachedClients: 0, order: 2 },
  { id: "$1", name: "one", windowCount: 1, attachedClients: 0, order: 1 },
];
const tabs: PersistedAppState = {
  ...defaultAppState,
  appTabs: [
    { id: "b", hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "one", kind: "gitDiff", resource: "/r/b", title: "b", order: 1 },
    { id: "a", hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "one", kind: "file", resource: "/r/a", title: "a", order: 0, rootPath: "/r", rootToken: "server-a-root" },
  ],
};

describe("application shell model", () => {
  it("orders sessions and keeps a stable selection through ID churn by unique name", () => {
    expect(orderedSessions(sessions).map((session) => session.id)).toEqual(["$1", "$2"]);
    expect(resolveSelectedSession(sessions, "$1", "one")?.id).toBe("$1");
    expect(resolveSelectedSession([{ ...sessions[1], id: "$9" }], "$1", "one")?.id).toBe("$9");
  });

  it("combines authoritative terminal tabs with ordered app tabs without persisting terminal entities", () => {
    const windows: TmuxWindow[] = [
      { id: "@2", sessionId: "$1", index: 2, name: "second", active: false, layout: "" },
      { id: "@1", sessionId: "$1", index: 1, name: "first", active: true, layout: "" },
    ];
    expect(combineWorkspaceTabs(windows, appTabsForWorkspace(tabs, "local", "server-a", sessions[1])).map((tab) => tab.key))
      .toEqual(["terminal:@1", "terminal:@2", "app:a", "app:b"]);
    const combined = combineWorkspaceTabs(windows, appTabsForWorkspace(tabs, "local", "server-a", sessions[1]));
    expect(combined[1]).toMatchObject({ key: "terminal:@2", canMoveRight: false });
    expect(combined[2]).toMatchObject({ key: "app:a", canMoveLeft: false });
  });

  it("shows a pending placeholder last, and retires it when its window arrives", () => {
    const windows: TmuxWindow[] = [
      { id: "@1", sessionId: "$1", index: 1, name: "first", active: true, layout: "" },
    ];
    const pending = { key: "create-window:1", sessionId: "$1", title: "New window" };

    // Last: a placeholder that pushed the existing tabs sideways would move
    // the targets under a waiting person's cursor.
    expect(combineWorkspaceTabs(windows, [], undefined, pending).map((tab) => tab.key))
      .toEqual(["terminal:@1", "pending:create-window:1"]);

    // Named by the ack but not yet in a snapshot: still shown, because
    // dropping it here blinks the strip empty until the snapshot lands.
    expect(combineWorkspaceTabs(windows, [], undefined, { ...pending, windowId: "@2" }).map((tab) => tab.key))
      .toEqual(["terminal:@1", "pending:create-window:1"]);

    // The real window arrived, so the placeholder retires rather than sitting
    // alongside the tab it stood in for.
    const settled: TmuxWindow[] = [
      ...windows,
      { id: "@2", sessionId: "$1", index: 2, name: "second", active: false, layout: "" },
    ];
    expect(combineWorkspaceTabs(settled, [], undefined, { ...pending, windowId: "@2" }).map((tab) => tab.key))
      .toEqual(["terminal:@1", "terminal:@2"]);
  });

  /**
   * The placeholder retires once and does not come back.
   *
   * Visibility alone is a live predicate, so an app-created window's
   * placeholder reappeared the moment that window was closed — an italic
   * "New window" tab standing for nothing, and undismissable: a placeholder has
   * no context menu, the close path returns early for it, and the bulk-close
   * helpers filter it out. The latch is what the shell steps its state through.
   */
  it("retires the placeholder for good once its window has been seen", () => {
    const before: TmuxWindow[] = [
      { id: "@1", sessionId: "$1", index: 1, name: "first", active: true, layout: "" },
    ];
    const after: TmuxWindow[] = [
      ...before,
      { id: "@2", sessionId: "$1", index: 2, name: "second", active: false, layout: "" },
    ];
    const acked = { key: "create-window:1", sessionId: "$1", windowId: "@2", title: "New window" };

    // The ack is not the snapshot: held, or the strip blinks empty.
    expect(retirePendingTab(acked, before)).toBe(acked);

    const retired = retirePendingTab(acked, after);
    expect(retired).toBeUndefined();

    // The window it stood in for is closed again. Nothing to resurrect.
    expect(retirePendingTab(retired, before)).toBeUndefined();
    expect(combineWorkspaceTabs(before, [], undefined, retired).map((tab) => tab.key)).toEqual(["terminal:@1"]);
  });

  it("reports a bulk close that left survivors, and says nothing when it did not", () => {
    expect(bulkCloseOutcomeStatus(7, 0)).toBeUndefined();
    expect(bulkCloseOutcomeStatus(0, 0)).toBeUndefined();
    expect(bulkCloseOutcomeStatus(7, 0, 0)).toBeUndefined();
    expect(bulkCloseOutcomeStatus(5, 2)).toBe("Closed 5 of 7 tabs; 2 could not be closed.");
    expect(bulkCloseOutcomeStatus(0, 1)).toBe("Closed 0 of 1 tab; 1 could not be closed.");
    // A tab the commit-time recheck held back is a survivor too. Saying nothing
    // about it read as a close that worked while the terminal was still there.
    expect(bulkCloseOutcomeStatus(0, 0, 1)).toBe("Closed 0 of 1 tab; 1 still had an agent and was left open.");
    expect(bulkCloseOutcomeStatus(3, 0, 2)).toBe("Closed 3 of 5 tabs; 2 still had an agent and were left open.");
    expect(bulkCloseOutcomeStatus(3, 1, 2))
      .toBe("Closed 3 of 6 tabs; 1 could not be closed; 2 still had an agent and were left open.");
  });

  it("keeps replaced servers isolated and requires explicit unambiguous app-tab recovery", () => {
    const migrated = reconcileWorkspaceIdentity(tabs, "local", "server-b", [{ ...sessions[1], id: "$99" }]);
    expect(migrated).toEqual(tabs);
    expect(appTabsForWorkspace(migrated, "local", "server-b", { ...sessions[1], id: "$99" })).toEqual([]);
    expect(appTabsForWorkspace(migrated, "local", undefined, sessions[1])).toEqual([]);
    expect(reconcileWorkspaceIdentity(tabs, "local", "server-b", []).appTabs).toEqual(tabs.appTabs);
    expect(recoverableAppTabCount(tabs, "local", "server-a", [{ ...sessions[1], id: "$99" }])).toBe(2);
    const rebound = recoverAppTabsFromPreviousServer(tabs, "local", "server-a", "server-b", [{ ...sessions[1], id: "$99" }]);
    expect(appTabsForWorkspace(rebound, "local", "server-b", { ...sessions[1], id: "$99" })).toHaveLength(2);
    expect(rebound.appTabs.find((tab) => tab.id === "a")).toMatchObject({ rootPath: undefined, rootToken: undefined });
    const ambiguous = recoverAppTabsFromPreviousServer(tabs, "local", "server-a", "server-b", [
      { ...sessions[1], id: "$98" }, { ...sessions[1], id: "$99" },
    ]);
    expect(ambiguous).toEqual(tabs);
    expect(discardServerAppState(tabs, "local", "server-a").appTabs).toEqual([]);
  });

  it("takes bulk closes from the strip's own order, never a placeholder or the anchor", () => {
    const windows: TmuxWindow[] = [
      { id: "@1", sessionId: "$1", index: 1, name: "first", active: true, layout: "" },
      { id: "@2", sessionId: "$1", index: 2, name: "second", active: false, layout: "" },
    ];
    const strip = combineWorkspaceTabs(
      windows,
      appTabsForWorkspace(tabs, "local", "server-a", sessions[1]),
      undefined,
      { key: "create-window:1", sessionId: "$1", title: "New window" },
    );
    expect(strip.map((tab) => tab.key)).toEqual(["terminal:@1", "terminal:@2", "app:a", "app:b", "pending:create-window:1"]);

    // Anchored first: everything else, minus the placeholder there is nothing
    // on the host to close.
    expect(tabsToCloseOthers(strip, "terminal:@1").map((tab) => tab.key)).toEqual(["terminal:@2", "app:a", "app:b"]);
    expect(tabsToCloseRight(strip, "terminal:@1").map((tab) => tab.key)).toEqual(["terminal:@2", "app:a", "app:b"]);
    // Anchored in the middle: "right" is a position in this array, so it takes
    // the app tabs after it and none of the terminal tabs before it.
    expect(tabsToCloseOthers(strip, "app:a").map((tab) => tab.key)).toEqual(["terminal:@1", "terminal:@2", "app:b"]);
    expect(tabsToCloseRight(strip, "app:a").map((tab) => tab.key)).toEqual(["app:b"]);
    // Anchored last among real tabs: nothing to the right but the placeholder,
    // which is not a target — so the item has nothing to do.
    expect(tabsToCloseRight(strip, "app:b")).toEqual([]);
    expect(tabsToCloseOthers(strip, "app:b").map((tab) => tab.key)).toEqual(["terminal:@1", "terminal:@2", "app:a"]);

    // A single tab is its own anchor: neither close has a subject.
    const alone: CombinedTab[] = [strip[0]];
    expect(tabsToCloseOthers(alone, "terminal:@1")).toEqual([]);
    expect(tabsToCloseRight(alone, "terminal:@1")).toEqual([]);
    // An anchor the strip no longer holds closes nothing, rather than closing
    // every tab because the exclusion matched none of them.
    expect(tabsToCloseOthers(strip, "terminal:@9")).toEqual([]);
    expect(tabsToCloseRight(strip, "terminal:@9")).toEqual([]);
  });

  it("closes all and only tabs without agents, protecting every agent state", () => {
    const windows: TmuxWindow[] = Array.from({ length: 6 }, (_, index) => ({
      id: `@${index + 1}`, sessionId: "$1", index: index + 1, name: `tab-${index + 1}`,
      active: index === 0, layout: "",
    }));
    const rollups = deriveAgentRollups([
      agent({ id: "working", windowId: "@1", lifecycle: "working" }),
      agent({ id: "blocked", windowId: "@2", lifecycle: "blocked", adapterId: "claude-code" }),
      agent({ id: "idle", windowId: "@3", lifecycle: "idle" }),
      agent({ id: "unknown", windowId: "@4", lifecycle: "unknown" }),
      agent({ id: "done", windowId: "@5", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2, seenGeneration: 1 }),
    ]);
    const current = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, topologyGeneration: 9,
    };
    const accepted = { ...current, coveredWindowIds: new Set(windows.map((window) => window.id)) };
    const strip = combineWorkspaceTabs(
      windows,
      appTabsForWorkspace(tabs, "local", "server-a", sessions[1]),
      rollups.byWindow,
      { key: "pending", sessionId: "$1", title: "Creating" },
      { accepted, current },
    );

    expect(strip.filter((tab) => tab.kind === "terminal").map((tab) => [tab.key, tab.agentPresence]))
      .toEqual([
        ["terminal:@1", "present"], ["terminal:@2", "present"], ["terminal:@3", "present"],
        ["terminal:@4", "present"], ["terminal:@5", "present"], ["terminal:@6", "absent"],
      ]);
    // Each tab also carries whose agent it is, so the strip can draw the mark
    // the sidebar draws; an empty window has nobody to name.
    expect(strip.filter((tab) => tab.kind === "terminal").map((tab) => tab.agentAdapterId))
      .toEqual(["codex", "claude-code", "codex", "codex", "codex", undefined]);
    expect(tabsToCloseNonAgent(strip).map((tab) => tab.key)).toEqual(["terminal:@6", "app:a", "app:b"]);
    expect(tabsToCloseNonAgent(strip.filter((tab) => tab.kind === "terminal" && tab.agentPresence === "present"))).toEqual([]);

    const unknown = combineWorkspaceTabs(windows, [], new Map(), undefined, {
      accepted,
      current: { ...current, topologyGeneration: 10 },
    });
    expect(unknown.every((tab) => tab.kind !== "terminal" || tab.agentPresence === "unknown")).toBe(true);
    expect(tabsToCloseNonAgent(unknown)).toEqual([]);

    const captured = tabsToCloseNonAgent(strip);
    const gainedAgent = deriveAgentRollups([agent({ id: "late", windowId: "@6", lifecycle: "working" })]);
    expect(tabsEligibleAtBulkCloseCommit(captured, true, {
      accepted: { ...accepted, coveredWindowIds: new Set(["@6"]) }, current, byWindow: gainedAgent.byWindow,
    })
      .map((tab) => tab.key)).toEqual(["app:a", "app:b"]);
    expect(tabsEligibleAtBulkCloseCommit(captured, true, {
      accepted, current: { ...current, topologyGeneration: 10 }, byWindow: new Map(),
    })
      .map((tab) => tab.key)).toEqual(["app:a", "app:b"]);
    expect(tabsEligibleAtBulkCloseCommit(captured, true, {
      accepted, current, byWindow: new Map(), hasUnmappedAgents: true,
    })
      .map((tab) => tab.key)).toEqual(["app:a", "app:b"]);
    expect(tabsEligibleAtBulkCloseCommit(captured, false, {
      byWindow: new Map(),
    }))
      .toEqual(captured);

    // The strip and the commit-time recheck read one snapshot. An authority
    // holding an agent it cannot place makes both say "unknown"; when the strip
    // was handed a narrowed authority it said "absent" and offered up a tab the
    // recheck then refused, which is a close that quietly did nothing.
    const unmapped = combineWorkspaceTabs(windows, [], rollups.byWindow, undefined, {
      accepted, current, hasUnmappedAgents: true,
    });
    expect(unmapped.every((tab) => tab.kind !== "terminal" || tab.agentPresence === "unknown")).toBe(true);
    expect(tabsToCloseNonAgent(unmapped)).toEqual([]);
  });

  it("strips known status tickers without depending on agent authority", () => {
    const windows: TmuxWindow[] = [
      { id: "@1", sessionId: "$1", index: 1, name: "✳ Fix tests", active: true, layout: "" },
      { id: "@2", sessionId: "$1", index: 2, name: "✓ Deploy checklist", active: false, layout: "" },
    ];
    const current = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, topologyGeneration: 9,
    };
    const accepted = { ...current, coveredWindowIds: new Set(["@1", "@2"]) };
    const rollups = deriveAgentRollups([agent({ id: "codex", windowId: "@1", lifecycle: "working" })]);
    const strip = combineWorkspaceTabs(windows, [], rollups.byWindow, undefined, { accepted, current });
    expect(strip.map((tab) => tab.title)).toEqual(["Fix tests", "Deploy checklist"]);

    // A title redraw temporarily makes agent authority stale. The presentation
    // stays identical instead of exposing the raw glyph until authority catches
    // up, which was the source of the visible flicker.
    expect(combineWorkspaceTabs(windows, [], rollups.byWindow).map((tab) => tab.title))
      .toEqual(["Fix tests", "Deploy checklist"]);
  });

  it("prunes app state only when its session is definitively absent from the same server", () => {
    expect(reconcileWorkspaceIdentity(tabs, "local", "server-a", []).appTabs).toEqual([]);
    expect(reconcileWorkspaceIdentity(tabs, "local", "server-b", []).appTabs).toEqual(tabs.appTabs);
  });

  it("persists selection, closes only app-owned state, and reorders only peer app tabs", () => {
    const selected = selectAppTab(tabs, "local", "server-a", sessions[1], "a");
    expect(selected.workspaceUi[0].selectedAppTabId).toBe("a");
    const moved = reorderAppTab(selected, "local", "server-a", sessions[1], "a", "right");
    expect(appTabsForWorkspace(moved, "local", "server-a", sessions[1]).map((tab) => tab.id)).toEqual(["b", "a"]);
    const closed = closeAppTab(moved, "local", "a");
    expect(closed.appTabs.map((tab) => tab.id)).toEqual(["b"]);
    expect(closed.workspaceUi[0].selectedAppTabId).toBeUndefined();
  });

  it("persists separate staged and unstaged Git diff identities across root changes", () => {
    const entry: GitStatusEntry = { path: "YSBmaWxl", displayPath: "a file", indexKind: "modified", worktreeKind: "modified", indexStatus: "M", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false };
    const status: GitStatusSnapshot = { repository: { id: "repo-id", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "7", sourceGeneration: "source", entries: [entry], authoritative: true };
    const staged = openGitDiffTab(tabs, "local", "server-a", sessions[1], entry, "staged", status, { path: "/repo", token: "root-token" });
    const unstaged = openGitDiffTab(staged, "local", "server-a", sessions[1], entry, "unstaged", status, { path: "/repo", token: "root-token" });
    expect(unstaged.appTabs.filter((tab) => tab.kind === "gitDiff")).toHaveLength(3);
    expect(unstaged.appTabs.filter((tab) => tab.gitRepositoryId === "repo-id")).toEqual(expect.arrayContaining([
      expect.objectContaining({ gitTarget: "staged", gitPath: "YSBmaWxl", rootPath: "/repo", rootToken: "root-token" }),
      expect.objectContaining({ gitTarget: "unstaged", gitPath: "YSBmaWxl", rootPath: "/repo", rootToken: "root-token" }),
    ]));
  });

  it("opens a single-clicked Git diff as transient, promotes it on a pinned open, and never demotes it", () => {
    const entry: GitStatusEntry = { path: "YSBmaWxl", displayPath: "a file", indexKind: "modified", worktreeKind: "modified", indexStatus: "M", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false };
    const status: GitStatusSnapshot = { repository: { id: "repo-id", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "7", sourceGeneration: "source", entries: [entry], authoritative: true };
    const root = { path: "/repo", token: "root-token" };
    const open = (state: PersistedAppState, preview: boolean) =>
      openGitDiffTab(state, "local", "server-a", sessions[1], entry, "unstaged", status, root, { preview });
    const find = (state: PersistedAppState) => findGitDiffTab(state, "local", "server-a", sessions[1].id, "repo-id", entry.path, "unstaged");

    const transient = open(defaultAppState, true);
    expect(transient.appTabs).toHaveLength(1);
    expect(find(transient)).toMatchObject({ kind: "gitDiff", preview: true });
    expect(transient.workspaceUi[0].selectedAppTabId).toBe(find(transient)!.id);

    // A second single click keeps the same transient tab.
    const again = open(transient, true);
    expect(again.appTabs).toHaveLength(1);
    expect(find(again)).toMatchObject({ id: find(transient)!.id, preview: true });

    // A pinned open promotes it in place; a later single click does not demote it.
    const pinned = open(again, false);
    expect(pinned.appTabs).toHaveLength(1);
    expect(find(pinned)!.id).toBe(find(transient)!.id);
    expect(find(pinned)!.preview).toBeUndefined();
    expect(find(open(pinned, true))!.preview).toBeUndefined();

    // A file preview does not take over the transient diff's slot: the diff
    // is closed by navigation, not rewritten into a file.
    const withFile = openFileTab(transient, "local", "server-a", sessions[1], "/repo/a.ts", "file", { path: "/repo", token: "root", revision: "1" }, { preview: true });
    expect(withFile.appTabs).toHaveLength(2);
    expect(find(withFile)).toMatchObject({ kind: "gitDiff", preview: true });

    // The strip's double-click pins a transient diff the same way.
    expect(find(pinAppTab(transient, "local", find(transient)!.id))!.preview).toBeUndefined();

    // Without the option the tab is pinned, as every caller before this option was.
    expect(find(openGitDiffTab(defaultAppState, "local", "server-a", sessions[1], entry, "unstaged", status, root))!.preview).toBeUndefined();
  });

  it("closes a transient diff on navigation away, and leaves a pinned one where it is", () => {
    const entry: GitStatusEntry = { path: "YSBmaWxl", displayPath: "a file", indexKind: "modified", worktreeKind: "modified", indexStatus: "M", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false };
    const status: GitStatusSnapshot = { repository: { id: "repo-id", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "7", sourceGeneration: "source", entries: [entry], authoritative: true };
    const transient = openGitDiffTab(tabs, "local", "server-a", sessions[1], entry, "unstaged", status, { path: "/repo", token: "t" }, { preview: true });
    const transientId = findGitDiffTab(transient, "local", "server-a", sessions[1].id, "repo-id", entry.path, "unstaged")!.id;
    const left = closeTransientGitDiff(transient, "local", transientId);
    expect(left.appTabs.map((tab) => tab.id)).toEqual(tabs.appTabs.map((tab) => tab.id));
    expect(left.workspaceUi[0].selectedAppTabId).toBeUndefined();

    const pinned = pinAppTab(transient, "local", transientId);
    expect(closeTransientGitDiff(pinned, "local", transientId)).toBe(pinned);
    // A file tab and an id that is already gone are not this rule's business.
    expect(closeTransientGitDiff(tabs, "local", "a")).toBe(tabs);
    expect(closeTransientGitDiff(left, "local", transientId)).toBe(left);
  });

  it("does not resurrect a closed transient diff when its open commit runs a second time", () => {
    const entry: GitStatusEntry = { path: "YSBmaWxl", displayPath: "a file", indexKind: "modified", worktreeKind: "modified", indexStatus: "M", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false };
    const status: GitStatusSnapshot = { repository: { id: "repo-id", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "7", sourceGeneration: "source", entries: [entry], authoritative: true };
    // The shape of the commit the shell hands the navigation coordinator: the
    // first commit opens, every later one only re-selects what is there. The
    // decision is made per commit, and the updater itself stays pure — React
    // runs it twice under StrictMode and keeps the second result.
    let committed = false;
    const find = (state: PersistedAppState) => findGitDiffTab(state, "local", "server-a", sessions[1].id, "repo-id", entry.path, "unstaged");
    const commit = (): ((current: PersistedAppState) => PersistedAppState) => {
      const replay = committed;
      committed = true;
      return (current) => {
        if (!replay) return openGitDiffTab(current, "local", "server-a", sessions[1], entry, "unstaged", status, { path: "/repo", token: "t" }, { preview: true });
        const opened = find(current);
        return opened ? selectAppTab(current, "local", "server-a", sessions[1], opened.id) : current;
      };
    };
    const first = commit();
    first(defaultAppState);
    const opened = first(defaultAppState);
    expect(opened.appTabs).toHaveLength(1);
    const closed = closeAppTab(opened, "local", find(opened)!.id);
    expect(closed.appTabs).toHaveLength(0);
    const replay = commit();
    expect(replay(closed)).toBe(closed);
    // A replay while the tab is still open only re-selects it.
    const reselected = replay(selectAppTab(opened, "local", "server-a", sessions[1], undefined));
    expect(reselected.appTabs).toHaveLength(1);
    expect(reselected.workspaceUi[0].selectedAppTabId).toBe(find(opened)!.id);
  });

  it("opens one root-scoped file tab and preserves it across root changes", () => {
    const opened = openFileTab(defaultAppState, "local", "server-a", sessions[1], "/repo/README.md", "markdown", { path: "/repo", token: "root-a", revision: "7" });
    expect(opened.appTabs).toHaveLength(1);
    expect(opened.appTabs[0]).toMatchObject({ rootPath: "/repo", rootToken: "root-a", viewMode: "split" });
    const deduplicated = openFileTab(opened, "local", "server-a", sessions[1], "/repo/README.md", "markdown", { path: "/other", token: "root-b", revision: "8" });
    expect(deduplicated.appTabs).toHaveLength(1);
    const preview = setMarkdownViewMode(deduplicated, "local", deduplicated.appTabs[0].id, "preview");
    expect(preview.appTabs[0].viewMode).toBe("preview");
  });

  it("refreshes an existing terminal-opened tab capability without resetting its view", () => {
    const opened = openFileTab(
      defaultAppState,
      "local",
      "server-a",
      sessions[1],
      "/tmp/scratchpad/prompt.md",
      "markdown",
      { path: "/tmp/scratchpad", token: "file-v1:old", revision: "1" },
    );
    const preview = setMarkdownViewMode(opened, "local", opened.appTabs[0].id, "preview");
    const refreshed = openFileTab(
      preview,
      "local",
      "server-a",
      sessions[1],
      "/tmp/scratchpad/prompt.md",
      "markdown",
      { path: "/tmp/scratchpad", token: "file-v1:new", revision: "2" },
      { preview: false, refreshRoot: true },
    );

    expect(refreshed.appTabs).toHaveLength(1);
    expect(refreshed.appTabs[0]).toMatchObject({
      rootPath: "/tmp/scratchpad",
      rootToken: "file-v1:new",
      viewMode: "preview",
    });
  });

  it("reacquires a cleared terminal-file capability after server recovery", () => {
    const opened = openFileTab(
      defaultAppState,
      "local",
      "server-a",
      sessions[1],
      "/tmp/scratchpad/prompt.md",
      "markdown",
      { path: "/tmp/scratchpad", token: "file-v1:old", revision: "1" },
    );
    const liveSession = { ...sessions[1], id: "$99" };
    const recovered = recoverAppTabsFromPreviousServer(
      opened,
      "local",
      "server-a",
      "server-b",
      [liveSession],
    );
    expect(recovered.appTabs[0]).toMatchObject({ rootPath: undefined, rootToken: undefined });

    const refreshed = openFileTab(
      recovered,
      "local",
      "server-b",
      liveSession,
      "/tmp/scratchpad/prompt.md",
      "markdown",
      { path: "/tmp/scratchpad", token: "file-v1:new", revision: "1" },
      { preview: false, refreshRoot: true },
    );
    expect(refreshed.appTabs[0]).toMatchObject({
      sessionId: "$99",
      rootPath: "/tmp/scratchpad",
      rootToken: "file-v1:new",
    });
  });

  /**
   * The configured mode is a *starting* mode. It is read when a tab is made and
   * never again, which is what keeps the two directions apart: changing the
   * setting leaves open tabs alone, and changing an open tab's mode leaves the
   * setting alone.
   */
  it("starts a new Markdown tab in the configured mode and leaves an open one where it is", () => {
    const open = (state: PersistedAppState, resource: string, viewMode?: "source" | "preview" | "split") =>
      openFileTab(state, "local", "server-a", sessions[1], resource, "markdown",
        { path: "/repo", token: "root", revision: "1" }, { preview: false, ...(viewMode ? { viewMode } : {}) });

    expect(open(defaultAppState, "/repo/a.md", "preview").appTabs[0].viewMode).toBe("preview");
    expect(open(defaultAppState, "/repo/a.md", "source").appTabs[0].viewMode).toBe("source");
    // Omitted is the behaviour every build before the setting had.
    expect(open(defaultAppState, "/repo/a.md").appTabs[0].viewMode).toBe("split");

    const opened = open(defaultAppState, "/repo/a.md", "source");
    const switched = setMarkdownViewMode(opened, "local", opened.appTabs[0].id, "split");
    // Reopening the same file with a different default does not restart it.
    expect(open(switched, "/repo/a.md", "preview").appTabs[0].viewMode).toBe("split");
  });

  it("keeps at most one preview tab per workspace and reuses its slot in place", () => {
    const open = (state: PersistedAppState, resource: string, preview: boolean, kind: "file" | "markdown" = "file") =>
      openFileTab(state, "local", "server-a", sessions[1], resource, kind, { path: "/repo", token: "root", revision: "1" }, { preview });

    const first = open(defaultAppState, "/repo/a.ts", true);
    expect(first.appTabs).toHaveLength(1);
    expect(first.appTabs[0].preview).toBe(true);

    // A second single click reuses the slot: same id, same order, new file.
    const second = open(first, "/repo/b.ts", true);
    expect(second.appTabs).toHaveLength(1);
    expect(second.appTabs[0]).toMatchObject({ id: first.appTabs[0].id, order: 0, resource: "/repo/b.ts", title: "b.ts", preview: true });
    expect(second.workspaceUi[0].selectedAppTabId).toBe(first.appTabs[0].id);

    // Reuse replaces the record, not merges into it: the previous file's
    // markdown view mode must not survive onto a .ts file.
    const markdownPreview = open(second, "/repo/README.md", true, "markdown");
    expect(markdownPreview.appTabs[0].viewMode).toBe("split");
    expect(open(markdownPreview, "/repo/c.ts", true).appTabs[0].viewMode).toBeUndefined();

    // A double-click on the same file pins the tab it already created, and the
    // pinned field goes away rather than becoming `false`.
    const pinned = open(open(second, "/repo/b.ts", true), "/repo/b.ts", false);
    expect(pinned.appTabs).toHaveLength(1);
    expect(pinned.appTabs[0].id).toBe(first.appTabs[0].id);
    expect("preview" in pinned.appTabs[0]).toBe(false);

    // With nothing disposable left, the next single click adds a tab.
    const third = open(pinned, "/repo/c.ts", true);
    expect(third.appTabs).toHaveLength(2);
    expect(third.appTabs.filter((tab) => tab.preview)).toHaveLength(1);
    expect(third.appTabs[1].order).toBe(1);

    // Reopening a pinned file as a preview must never demote it back to
    // disposable, and must not touch the workspace's actual preview tab.
    const reopened = open(third, "/repo/b.ts", true);
    expect(reopened.appTabs.find((tab) => tab.resource === "/repo/b.ts")?.preview).toBeUndefined();
    expect(reopened.appTabs.filter((tab) => tab.preview).map((tab) => tab.resource)).toEqual(["/repo/c.ts"]);

    // A preview tab belongs to its own workspace: another session's click gets
    // its own slot rather than stealing this one.
    const otherSession = openFileTab(third, "local", "server-a", sessions[0], "/repo/d.ts", "file", { path: "/repo", token: "root", revision: "1" }, { preview: true });
    expect(otherSession.appTabs).toHaveLength(3);
    expect(otherSession.appTabs.filter((tab) => tab.preview)).toHaveLength(2);

    // The strip reads it, and `pinAppTab` is idempotent because the editor
    // calls it on every keystroke.
    expect(combineWorkspaceTabs([], appTabsForWorkspace(third, "local", "server-a", sessions[1]))
      .map((tab) => tab.kind === "app" && tab.preview)).toEqual([false, true]);
    const editPinned = pinAppTab(third, "local", third.appTabs[1].id);
    expect(editPinned.appTabs[1].preview).toBeUndefined();
    expect(pinAppTab(editPinned, "local", editPinned.appTabs[1].id)).toBe(editPinned);
    expect(pinAppTab(third, "local", "no-such-tab")).toBe(third);
    expect(pinAppTab(third, "other-host", third.appTabs[1].id)).toBe(third);
  });

  it("opens a permanent tab by default, so no caller gets a disposable one by omission", () => {
    const opened = openFileTab(defaultAppState, "local", "server-a", sessions[1], "/repo/a.ts", "file", { path: "/repo", token: "root", revision: "1" });
    expect(opened.appTabs[0].preview).toBeUndefined();
  });

  it("relocates open file tabs after an app-owned file or directory move", () => {
    const file = openFileTab(defaultAppState, "local", "server-a", sessions[1], "/repo/old/a.txt", "file", { path: "/repo", token: "root", revision: "1" });
    const markdown = openFileTab(file, "local", "server-a", sessions[1], "/repo/old/guide.md", "markdown", { path: "/repo", token: "root", revision: "1" });
    const moved = relocateFileTabs(markdown, "local", "server-a", "/repo", "/repo/old", "new 日本語");
    expect(moved.appTabs).toEqual(expect.arrayContaining([
      expect.objectContaining({ resource: "/repo/new 日本語/a.txt", title: "a.txt" }),
      expect.objectContaining({ resource: "/repo/new 日本語/guide.md", title: "guide.md" }),
    ]));
    const normalized = relocateFileTabs(moved, "local", "server-a", "/repo/", "/repo/new 日本語/a.txt", "./final//a.txt");
    expect(normalized.appTabs).toEqual(expect.arrayContaining([
      expect.objectContaining({ resource: "/repo/final/a.txt", title: "a.txt" }),
    ]));
    expect(relocateFileTabs(moved, "local", "other-server", "/repo", "/repo/new 日本語/a.txt", "/repo/nope")).toBe(moved);
  });

  it("routes agent shell selections only through exact stable topology IDs", () => {
    const snapshot: TmuxSnapshot = {
      sessions: [sessions[1]],
      windows: [{ id: "@1", sessionId: "$1", index: 1, name: "shell", active: true, layout: "" }],
      panes: [{ id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/r", currentCommand: "bash" }],
    };
    const item = { id: "agent", label: "Agent", state: "working" as const, hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@1", paneId: "%1", updatedAt: 1 };
    expect(resolveAgentShellDestination(snapshot, item, "local", "server-a")?.id).toBe("%1");
    expect(resolveAgentShellDestination(snapshot, { ...item, windowId: "@9" }, "local", "server-a")).toBeUndefined();
    expect(resolveAgentShellDestination(snapshot, item, "ssh:other", "server-a")).toBeUndefined();
    expect(resolveAgentShellDestination(snapshot, item, "local", "server-b")).toBeUndefined();
  });

  it("keeps large rails and tab sets deterministic and keeps the active window's panes mounted behind app tabs", () => {
    const largeSessions = Array.from({ length: 20 }, (_, index): Session => ({
      id: `$${index}`, name: `session-${String(index).padStart(2, "0")}`, windowCount: 5, attachedClients: 0, order: 19 - index,
    }));
    expect(orderedSessions(largeSessions)).toHaveLength(20);
    expect(orderedSessions(largeSessions)[0].id).toBe("$19");
    const windows = Array.from({ length: 100 }, (_, index): TmuxWindow => ({
      id: `@${index}`, sessionId: "$1", index, name: `window-${index}`, active: index === 50, layout: "",
    }));
    expect(combineWorkspaceTabs(windows, tabs.appTabs)).toHaveLength(102);
    const panes = Array.from({ length: 50 }, (_, index) => ({
      id: `%${index}`, sessionId: "$1", windowId: "@50", index, active: index === 4,
      width: 10, height: 10, left: index * 10, top: 0, currentPath: "/r", currentCommand: "bash",
    }));
    // An app tab covers the terminal layer rather than replacing it, so the
    // active window's panes stay mounted underneath it.
    expect(mountedTerminalPanes(panes, "@50", false)).toHaveLength(50);
    expect(mountedTerminalPanes(panes, "@50", true).map((pane) => pane.id)).toEqual(["%4"]);
    // A window with no identity is the only thing that mounts nothing.
    expect(mountedTerminalPanes(panes, undefined, false)).toEqual([]);
  });

  it("keeps the last few selected document tabs mounted, most recent first", () => {
    // Selection promotes, and a re-selection is idempotent — this runs during
    // render, so the second call of a double render must not reorder anything.
    expect(mountedAppTabIds([], "a", 4)).toEqual(["a"]);
    expect(mountedAppTabIds(["a"], "b", 4)).toEqual(["b", "a"]);
    expect(mountedAppTabIds(["b", "a"], "a", 4)).toEqual(["a", "b"]);
    expect(mountedAppTabIds(["a", "b"], "a", 4)).toEqual(["a", "b"]);

    // The cap drops the least recently selected, never the selected one.
    expect(mountedAppTabIds(["d", "c", "b", "a"], "e", 4)).toEqual(["e", "d", "c", "b"]);
    expect(mountedAppTabIds(["b", "a"], "c", 1)).toEqual(["c"]);
    // A nonsensical limit still cannot unmount the tab being looked at.
    expect(mountedAppTabIds(["b", "a"], "c", 0)).toEqual(["c"]);

    // A terminal tab is selected: the documents already mounted stay mounted,
    // which is what makes going back to one a repaint.
    expect(mountedAppTabIds(["b", "a"], undefined, 4)).toEqual(["b", "a"]);

    // Ids the caller has already filtered — a closed or evicted tab — simply
    // are not there; the rule never re-adds them.
    expect(mountedAppTabIds(["b", "a"].filter((id) => id !== "b"), undefined, 4)).toEqual(["a"]);

    // The input is never mutated: it is the previous render's list.
    const previous = ["a", "b"];
    mountedAppTabIds(previous, "c", 2);
    expect(previous).toEqual(["a", "b"]);
  });

  it("surfaces a terminal only for a real authoritative focus transition", () => {
    expect(shouldSurfaceAuthoritativeTerminal(true, "$1", "@1", "$1", "@2")).toBe(true);
    expect(shouldSurfaceAuthoritativeTerminal(true, "$1", "@1", "$2", "@2")).toBe(false);
    expect(shouldSurfaceAuthoritativeTerminal(true, "$1", "@1", "$1", "@1")).toBe(false);
    expect(shouldSurfaceAuthoritativeTerminal(false, "$1", "@1", "$1", "@2")).toBe(false);
    expect(shouldSurfaceAuthoritativeTerminal(true, undefined, undefined, "$1", "@1")).toBe(false);
  });

  it("keeps navigation local while writes are frozen instead of queuing a tmux mutation", () => {
    expect(shellNavigationMode(true)).toBe("authoritative");
    expect(shellNavigationMode(false)).toBe("cached");
  });
});

describe("archived workspaces", () => {
  const archived = archiveWorkspace(defaultAppState, "local", "server-a", sessions[1], 10);

  it("hides the workspace on exactly this host and server, and unarchive restores it", () => {
    expect(archivedSessionIds(archived, "local", "server-a")).toEqual(new Set(["$1"]));
    expect(archivedWorkspacesFor(archived, "local", "server-a", sessions).map((session) => session.id)).toEqual(["$1"]);
    expect(archived.archivedWorkspaces).toEqual([
      { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "one", archivedAt: 10 },
    ]);
    // Idempotent, and never without a server identity to key on.
    expect(archiveWorkspace(archived, "local", "server-a", sessions[1], 11)).toBe(archived);
    expect(archiveWorkspace(defaultAppState, "local", undefined, sessions[1], 11)).toBe(defaultAppState);
    const restored = unarchiveWorkspace(archived, "local", "server-a", "$1");
    expect(restored.archivedWorkspaces).toEqual([]);
    expect(archivedSessionIds(restored, "local", "server-a").size).toBe(0);
    expect(unarchiveWorkspace(restored, "local", "server-a", "$1")).toBe(restored);
  });

  it("never lets a record from another server or host hide a workspace", () => {
    expect(archivedSessionIds(archived, "local", "server-b").size).toBe(0);
    expect(archivedSessionIds(archived, "remote", "server-a").size).toBe(0);
    expect(archivedSessionIds(archived, "local", undefined).size).toBe(0);
    expect(archivedWorkspacesFor(archived, "local", "server-b", sessions)).toEqual([]);
    // Another server's records are untouched by this server's reconcile…
    expect(reconcileWorkspaceIdentity(archived, "local", "server-b", [])).toBe(archived);
    // …and gone when that server's state is discarded.
    expect(discardServerAppState(archived, "local", "server-a").archivedWorkspaces).toEqual([]);
  });

  it("follows a rename, survives a reconnect, and forgets a session killed from another client", () => {
    const renamed = reconcileWorkspaceIdentity(archived, "local", "server-a", [{ ...sessions[1], name: "uno" }, sessions[0]]);
    expect(renamed.archivedWorkspaces[0]).toMatchObject({ sessionId: "$1", sessionName: "uno" });
    expect(archivedSessionIds(renamed, "local", "server-a")).toEqual(new Set(["$1"]));
    // Same server, same sessions: nothing to change, same object.
    expect(reconcileWorkspaceIdentity(archived, "local", "server-a", sessions)).toBe(archived);
    const killed = reconcileWorkspaceIdentity(archived, "local", "server-a", [sessions[0]]);
    expect(killed.archivedWorkspaces).toEqual([]);
    // A record with no live session is not offered for unarchiving either.
    expect(archivedWorkspacesFor(archived, "local", "server-a", [sessions[0]])).toEqual([]);
  });

  it("caps the archive at 200, dropping the oldest", () => {
    let state = defaultAppState;
    for (let index = 0; index < 200; index += 1) {
      state = archiveWorkspace(state, "local", "server-a", { ...sessions[1], id: `$${index + 100}` }, index + 1);
    }
    expect(state.archivedWorkspaces).toHaveLength(200);
    const overflow = archiveWorkspace(state, "local", "server-a", { ...sessions[1], id: "$999" }, 500);
    expect(overflow.archivedWorkspaces).toHaveLength(200);
    expect(archivedSessionIds(overflow, "local", "server-a").has("$100")).toBe(false);
    expect(archivedSessionIds(overflow, "local", "server-a").has("$999")).toBe(true);
  });

  it("moves the selection off an archived workspace and onto the first visible one", () => {
    const hidden = new Set(["$1"]);
    expect(resolveSelectedSession(sessions, "$1", "one", hidden)?.id).toBe("$2");
    expect(resolveSelectedSession(sessions, undefined, "one", hidden)?.id).toBe("$2");
    expect(resolveSelectedSession(sessions, "$2", "two", hidden)?.id).toBe("$2");
    expect(resolveSelectedSession(sessions, "$1", "one", new Set(["$1", "$2"]))).toBeUndefined();
  });
});
