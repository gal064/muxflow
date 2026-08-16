import { describe, expect, it, vi } from "vitest";
import { GitRepositoryStore } from "./repositoryStore";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitDiff, GitStatusSnapshot, GitWorkspaceClient, GitWorkspaceEvent } from "./types";

describe("GitRepositoryStore", () => {
  it("gives every consumer of one repository the same watch and status", async () => {
    const { client, calls } = stubClient();
    const store = new GitRepositoryStore(client);
    const sidebar = store.acquire(scope, root);
    await flush();
    expect(calls.watch).toBe(1);

    const diffTab = store.acquire(scope, root);
    const secondDiffTab = store.acquire(scope, root);
    await flush();
    // A warm panel and a second matching diff tab cost nothing: no watch, no
    // status, no discovery.
    expect(calls.watch).toBe(1);
    expect(calls.status).toBe(0);
    expect(diffTab.state().status?.generation).toBe("1");
    expect(secondDiffTab.state().status?.generation).toBe("1");

    diffTab.release();
    secondDiffTab.release();
    await flush();
    expect(calls.release).toBe(0);
    sidebar.release();
    await flush();
    // The final release leaves no watch behind.
    expect(calls.release).toBe(1);
  });

  it("paints a re-acquired repository from its remembered status before the watch returns", async () => {
    const { client, calls } = stubClient();
    const store = new GitRepositoryStore(client);
    const first = store.acquire(scope, root);
    await flush();
    first.release();
    await flush();

    expect(store.peek(scope, root)?.status?.generation).toBe("1");
    const reopened = store.acquire(scope, root);
    expect(reopened.state().status?.generation).toBe("1");
    expect(calls.watch).toBe(2);
    reopened.release();
  });

  it("coalesces identical diff requests and drops them when the status moves on", async () => {
    const { client, calls, publish } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();

    const request = { repositoryId: "repo", path: "YQ==", target: "unstaged" as const };
    const [first, second] = await Promise.all([handle.diff(request), handle.diff(request)]);
    expect(calls.diff).toBe(1);
    expect(first).toBe(second);
    await handle.diff(request);
    expect(calls.diff).toBe(1);

    publish(snapshot("2"));
    await flush();
    await handle.diff(request);
    expect(calls.diff).toBe(2);
    handle.release();
  });

  it("treats an identical authoritative status as no transition at all", async () => {
    const { client, publish } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    const listener = vi.fn();
    handle.subscribe(listener);

    // A mutation delivers its status in its own response; the shared watch then
    // echoes the same snapshot. That is one transition, not two.
    handle.accept(snapshot("1"));
    publish(snapshot("1"));
    await flush();
    expect(listener).not.toHaveBeenCalled();

    handle.accept(snapshot("2"));
    expect(listener).toHaveBeenCalledTimes(1);
    handle.release();
  });

  it("never regresses to an older generation", async () => {
    const { client } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    handle.accept(snapshot("5"));
    handle.accept(snapshot("3"));
    expect(handle.state().status?.generation).toBe("5");
    handle.release();
  });

  it("ignores events belonging to another watch or another root", async () => {
    const { client, emit } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    emit({ kind: "status", rootToken: "other-root", watchId: "watch", status: snapshot("9") });
    emit({ kind: "status", rootToken: root.token, watchId: "someone-else", status: snapshot("9") });
    await flush();
    expect(handle.state().status?.generation).toBe("1");
    handle.release();
  });
});

const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 4, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "root-token", path: "/repo", cwd: "/repo", paneId: "%1", gitWorktree: true, revision: "1" };

function snapshot(generation: string): GitStatusSnapshot {
  return {
    repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" },
    generation,
    sourceGeneration: `source-${generation}`,
    entries: [],
    authoritative: true,
  };
}

function stubClient() {
  const calls = { watch: 0, status: 0, diff: 0, release: 0 };
  let listener: ((event: GitWorkspaceEvent) => void) | undefined;
  const client: GitWorkspaceClient = {
    status: vi.fn(async () => { calls.status += 1; return snapshot("1"); }),
    watch: vi.fn(async () => {
      calls.watch += 1;
      return {
        watchId: "watch",
        rootToken: root.token,
        connectionEpoch: scope.terminalEpoch,
        status: snapshot("1"),
        release: () => { calls.release += 1; },
      };
    }),
    diff: vi.fn(async () => {
      calls.diff += 1;
      return { diff: stubDiff(), status: snapshot("1") };
    }),
    prepareDiscard: vi.fn(),
    mutate: vi.fn(),
    commit: vi.fn(),
    subscribe: vi.fn((next) => { listener = next; return () => { listener = undefined; }; }),
  };
  return {
    client,
    calls,
    emit: (event: GitWorkspaceEvent) => listener?.(event),
    publish: (status: GitStatusSnapshot) => listener?.({ kind: "status", rootToken: root.token, watchId: "watch", status }),
  };
}

function stubDiff(): GitDiff {
  return {
    repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false },
    target: "unstaged", path: "YQ==", displayPath: "a", sourceGeneration: "s",
    binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 1,
  };
}

async function flush() {
  for (let turn = 0; turn < 4; turn += 1) await Promise.resolve();
}
