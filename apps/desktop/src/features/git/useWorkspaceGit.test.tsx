// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import { GitRepositoryStore } from "./repositoryStore";
import type { GitStatusSnapshot, GitWorkspaceClient, GitWorkspaceEvent } from "./types";
import { useWorkspaceGit } from "./useWorkspaceGit";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("useWorkspaceGit", () => {
  it("drops status from a previous root and accepts monotonic live generations", async () => {
    let listener: ((event: GitWorkspaceEvent) => void) | undefined;
    const rootOne = root("one", "/one");
    const rootTwo = root("two", "/two");
    const client: GitWorkspaceClient = {
      status: vi.fn(),
      watch: vi.fn(async (activeScope, active) => ({ watchId: `watch-${active.token}`, rootToken: active.token, connectionEpoch: activeScope.terminalEpoch, status: snapshot(active.token, "1"), release: vi.fn() })),
      diff: vi.fn(), prepareDiscard: vi.fn(), mutate: vi.fn(), commit: vi.fn(),
      subscribe: vi.fn((next) => { listener = next; return () => undefined; }),
    };
    const store = new GitRepositoryStore(client);
    let current: ReturnType<typeof useWorkspaceGit> | undefined;
    function Harness({ active }: { active: ActiveRoot }) { current = useWorkspaceGit(store, scope, active); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness active={rootOne} />); await Promise.resolve(); });
    await act(async () => { renderer.update(<Harness active={rootTwo} />); await Promise.resolve(); });
    expect(current?.status?.repository.id).toBe("two");
    await act(async () => { listener?.({ kind: "status", rootToken: "one", watchId: "watch-one", status: snapshot("one", "99") }); });
    expect(current?.status?.repository.id).toBe("two");
    await act(async () => { listener?.({ kind: "status", rootToken: "two", watchId: "old-watch-two", status: snapshot("two", "99") }); });
    expect(current?.status?.generation).toBe("1");
    await act(async () => { listener?.({ kind: "status", rootToken: "two", watchId: "watch-two", status: snapshot("two", "3") }); });
    expect(current?.status?.generation).toBe("3");
    await act(async () => { listener?.({ kind: "status", rootToken: "two", watchId: "watch-two", status: snapshot("two", "2") }); });
    expect(current?.status?.generation).toBe("3");
    await act(async () => { renderer.unmount(); });
  });

  it("rejects a late lease and event from an earlier connection epoch on the same root", async () => {
    const listeners: Array<(event: GitWorkspaceEvent) => void> = [];
    let resolveOld!: (lease: Awaited<ReturnType<GitWorkspaceClient["watch"]>>) => void;
    const oldLease = new Promise<Awaited<ReturnType<GitWorkspaceClient["watch"]>>>((resolve) => { resolveOld = resolve; });
    const releaseOld = vi.fn();
    const active = root("same", "/same");
    const client: GitWorkspaceClient = {
      status: vi.fn(),
      watch: vi.fn(async (activeScope) => activeScope.terminalEpoch === 1 ? oldLease : {
        watchId: "current-watch", rootToken: active.token, connectionEpoch: 2, status: snapshot("same", "2"), release: vi.fn(),
      }),
      diff: vi.fn(), prepareDiscard: vi.fn(), mutate: vi.fn(), commit: vi.fn(),
      subscribe: vi.fn((next) => { listeners.push(next); return () => undefined; }),
    };
    const store = new GitRepositoryStore(client);
    let current: ReturnType<typeof useWorkspaceGit> | undefined;
    function Harness({ activeScope }: { activeScope: FileWorkspaceScope }) { current = useWorkspaceGit(store, activeScope, active); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness activeScope={scope} />); await Promise.resolve(); });
    await act(async () => { renderer.update(<Harness activeScope={{ ...scope, terminalEpoch: 2 }} />); await Promise.resolve(); });
    expect(current?.status?.generation).toBe("2");
    await act(async () => {
      resolveOld({ watchId: "old-watch", rootToken: active.token, connectionEpoch: 1, status: snapshot("same", "99"), release: releaseOld });
      await Promise.resolve();
      for (const listener of listeners) listener({ kind: "status", rootToken: active.token, watchId: "old-watch", status: snapshot("same", "100") });
    });
    expect(releaseOld).toHaveBeenCalledOnce();
    expect(current?.status?.generation).toBe("2");
    await act(async () => { renderer.unmount(); });
  });

  it("reconciles a newer matching event delivered before the watch bootstrap resolves", async () => {
    let listener: ((event: GitWorkspaceEvent) => void) | undefined;
    let resolveWatch!: (lease: Awaited<ReturnType<GitWorkspaceClient["watch"]>>) => void;
    const watch = new Promise<Awaited<ReturnType<GitWorkspaceClient["watch"]>>>((resolve) => { resolveWatch = resolve; });
    const active = root("same", "/same");
    const client: GitWorkspaceClient = {
      status: vi.fn(), watch: vi.fn(() => watch), diff: vi.fn(), prepareDiscard: vi.fn(), mutate: vi.fn(), commit: vi.fn(),
      subscribe: vi.fn((next) => { listener = next; return () => undefined; }),
    };
    const store = new GitRepositoryStore(client);
    let current: ReturnType<typeof useWorkspaceGit> | undefined;
    function Harness() { current = useWorkspaceGit(store, scope, active); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => {
      resolveWatch({ watchId: "watch", rootToken: "same", connectionEpoch: 1, status: snapshot("same", "1"), release: vi.fn() });
      await Promise.resolve();
      listener?.({ kind: "status", rootToken: "same", watchId: "watch", status: snapshot("same", "2") });
    });
    expect(current?.status?.generation).toBe("2");
    await act(async () => { renderer.unmount(); });
  });

  it("holds one observation across a pane switch within the same worktree", async () => {
    // The panel used to blank on every pane focus and terminal tab switch, and
    // the cause was upstream of this hook: `useWorkspaceFiles` masked its whole
    // visible state on a selection change, so `root` flapped to `undefined`,
    // `observable` went false, and the panel fell back to IDLE for a round trip
    // before the identical repository came back. The files hook keeps painting
    // its root through that window now, and this is the other half of the
    // contract — the pane is deliberately not part of the observation's
    // identity, so the same worktree under a different pane is the same entry.
    const active = root("shared", "/shared");
    const watch = vi.fn(async () => ({ watchId: "watch", rootToken: active.token, connectionEpoch: scope.terminalEpoch, status: snapshot("shared", "1"), release: vi.fn() }));
    const client: GitWorkspaceClient = {
      status: vi.fn(), watch, diff: vi.fn(), prepareDiscard: vi.fn(), mutate: vi.fn(), commit: vi.fn(),
      subscribe: vi.fn(() => () => undefined),
    };
    const store = new GitRepositoryStore(client);
    let current: ReturnType<typeof useWorkspaceGit> | undefined;
    const seen: Array<string | undefined> = [];
    function Harness({ activeScope, activeRoot }: { activeScope: FileWorkspaceScope; activeRoot: ActiveRoot }) {
      current = useWorkspaceGit(store, activeScope, activeRoot);
      seen.push(current.status?.repository.id);
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness activeScope={scope} activeRoot={active} />); await Promise.resolve(); });
    expect(current?.status?.generation).toBe("1");

    // Exactly what the files hook publishes on the far side of a same-root
    // pane switch: the same capability, a new `paneId`.
    seen.length = 0;
    await act(async () => {
      renderer.update(<Harness activeScope={{ ...scope, paneId: "%2" }} activeRoot={{ ...active, paneId: "%2" }} />);
      await Promise.resolve();
    });
    expect(current?.status?.repository.id, "the panel dropped to IDLE on a pane switch").toBe("shared");
    expect(seen.every((id) => id === "shared"), "the panel blanked for at least one commit").toBe(true);
    expect(watch, "the pane switch re-observed a repository it was already watching").toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });
  });

  it("opens a panel beside a live diff tab with no request of its own", async () => {
    const active = root("shared", "/shared");
    const watch = vi.fn(async () => ({ watchId: "watch", rootToken: active.token, connectionEpoch: scope.terminalEpoch, status: snapshot("shared", "1"), release: vi.fn() }));
    const status = vi.fn();
    const client: GitWorkspaceClient = {
      status, watch, diff: vi.fn(), prepareDiscard: vi.fn(), mutate: vi.fn(), commit: vi.fn(),
      subscribe: vi.fn(() => () => undefined),
    };
    const store = new GitRepositoryStore(client);
    // A diff tab already observes this repository.
    const diffTab = store.acquire(scope, active);
    await act(async () => { await Promise.resolve(); });
    expect(watch).toHaveBeenCalledTimes(1);

    let current: ReturnType<typeof useWorkspaceGit> | undefined;
    function Harness() { current = useWorkspaceGit(store, scope, active); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    expect(current?.status?.generation).toBe("1");
    expect(current?.loading).toBe(false);
    expect(watch).toHaveBeenCalledTimes(1);
    expect(status).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
    diffTab.release();
  });
});

const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 1, sessionId: "$1", paneId: "%1" };
function root(token: string, path: string): ActiveRoot { return { token, path, cwd: path, paneId: "%1", gitWorktree: true, revision: "1" }; }
function snapshot(id: string, generation: string): GitStatusSnapshot { return { repository: { id, worktreeRoot: `/${id}`, initial: false, detachedHead: false, headName: "main" }, generation, sourceGeneration: generation, entries: [], authoritative: true }; }
