import { describe, expect, it } from "vitest";
import type { Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import {
  appTabsForWorkspace,
  closeAppTab,
  combineWorkspaceTabs,
  discardServerAppState,
  mountedTerminalPanes,
  openFileTab,
  openGitDiffTab,
  orderedSessions,
  pinAppTab,
  relocateFileTabs,
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
} from "./model";
import { defaultAppState, type PersistedAppState } from "./types";
import type { GitStatusEntry, GitStatusSnapshot } from "../git/types";

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

  it("opens one root-scoped file tab and preserves it across root changes", () => {
    const opened = openFileTab(defaultAppState, "local", "server-a", sessions[1], "/repo/README.md", "markdown", { path: "/repo", token: "root-a", revision: "7" });
    expect(opened.appTabs).toHaveLength(1);
    expect(opened.appTabs[0]).toMatchObject({ rootPath: "/repo", rootToken: "root-a", viewMode: "split" });
    const deduplicated = openFileTab(opened, "local", "server-a", sessions[1], "/repo/README.md", "markdown", { path: "/other", token: "root-b", revision: "8" });
    expect(deduplicated.appTabs).toHaveLength(1);
    const preview = setMarkdownViewMode(deduplicated, "local", deduplicated.appTabs[0].id, "preview");
    expect(preview.appTabs[0].viewMode).toBe("preview");
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

  it("keeps large rails and tab sets deterministic and mounts no terminal resources behind app tabs", () => {
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
    expect(mountedTerminalPanes(panes, "@50", true, false)).toEqual([]);
    expect(mountedTerminalPanes(panes, "@50", false, true).map((pane) => pane.id)).toEqual(["%4"]);
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
