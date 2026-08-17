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
    expect(diffTab.handle.state().status?.generation).toBe("1");
    expect(secondDiffTab.handle.state().status?.generation).toBe("1");

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
    expect(reopened.handle.state().status?.generation).toBe("1");
    expect(calls.watch).toBe(2);
    reopened.release();
  });

  it("shares one request between simultaneous callers and re-reads afterwards", async () => {
    const { client, calls } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();

    const request = { repositoryId: "repo", path: "YQ==", target: "unstaged" as const };
    const [first, second] = await Promise.all([handle.handle.diff(request), handle.handle.diff(request)]);
    expect(calls.diff).toBe(1);
    expect(first).toBe(second);

    // A resolved diff is never replayed: the host is the authority on what a
    // file currently looks like, so an explicit re-read must reach it.
    await handle.handle.diff(request);
    expect(calls.diff).toBe(2);
    handle.release();
  });

  it("cancels a diff only when every caller waiting on it has gone", async () => {
    const { client, calls } = stubClient();
    let observed: AbortSignal | undefined;
    vi.mocked(client.diff).mockImplementation(async (_scope, _root, _id, _path, _original, _target, signal) => {
      calls.diff += 1;
      observed = signal;
      return { diff: stubDiff(), status: snapshot("1") };
    });
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();

    const request = { repositoryId: "repo", path: "YQ==", target: "unstaged" as const };
    const leaving = new AbortController();
    const staying = new AbortController();
    const abandoned = handle.handle.diff(request, leaving.signal);
    const wanted = handle.handle.diff(request, staying.signal);
    expect(calls.diff).toBe(1);
    leaving.abort();
    expect(observed?.aborted).toBe(false);
    await expect(wanted).resolves.toBeDefined();
    await abandoned.catch(() => undefined);
    handle.release();
  });

  it("treats an identical authoritative status as no transition at all", async () => {
    const { client, publish } = stubClient();
    vi.mocked(client.mutate).mockResolvedValue({
      exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "",
      outcome: "applied", status: snapshot("1"),
    });
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    const listener = vi.fn();
    handle.handle.subscribe(listener);

    // A mutation delivers its status in its own response; the shared watch then
    // echoes the same snapshot. That is one transition, not two.
    await handle.handle.mutate("repo", stageRequest);
    publish(snapshot("1"));
    await flush();
    expect(listener).not.toHaveBeenCalled();

    publish(snapshot("2"));
    await flush();
    expect(listener).toHaveBeenCalledTimes(1);
    handle.release();
  });

  it("stops saying it is loading when an explicit refresh changed nothing", async () => {
    const { client } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    expect(handle.handle.state().loading).toBe(false);

    // The common case: the user presses refresh and the repository is exactly
    // as it was. That is still an answer, and the panel must settle on it.
    await handle.handle.refresh();
    await flush();
    expect(handle.handle.state().loading).toBe(false);
    expect(handle.handle.state().status?.generation).toBe("1");
    handle.release();
  });

  it("clears a watch error as soon as any status comes back", async () => {
    const { client, emit, publish } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    emit({ kind: "error", rootToken: root.token, watchId: "watch", error: "transient failure" });
    await flush();
    expect(handle.handle.state().error).toBe("transient failure");

    // The identical snapshot, republished. Recovery is a transition even when
    // the repository state is byte-identical.
    publish(snapshot("1"));
    await flush();
    expect(handle.handle.state().error).toBeUndefined();
    handle.release();
  });

  it("reconciles a mutation's authoritative status without a second request", async () => {
    const { client, calls } = stubClient();
    vi.mocked(client.mutate).mockResolvedValue({
      exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "",
      outcome: "applied", status: snapshot("2"),
    });
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    await handle.handle.mutate("repo", stageRequest);
    expect(handle.handle.state().status?.generation).toBe("2");
    expect(calls.status).toBe(0);
    handle.release();
  });

  it("never regresses to an older generation", async () => {
    const { client, publish } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    publish(snapshot("5"));
    publish(snapshot("3"));
    await flush();
    expect(handle.handle.state().status?.generation).toBe("5");
    handle.release();
  });

  it("does not open a second watch while one is still in flight", async () => {
    const { client, calls } = stubClient();
    let settleWatch!: () => void;
    vi.mocked(client.watch).mockImplementation(() => new Promise((resolve) => {
      calls.watch += 1;
      settleWatch = () => resolve({
        watchId: "watch", rootToken: root.token, connectionEpoch: scope.terminalEpoch,
        status: snapshot("1"), release: () => { calls.release += 1; },
      });
    }));
    const store = new GitRepositoryStore(client);
    const lease = store.acquire(scope, root);
    await flush();
    expect(calls.watch).toBe(1);

    // A refresh while the bootstrap is still pending must not start a second
    // one: whichever lease lost the race would never be released.
    void lease.handle.refresh();
    void lease.handle.refresh();
    await flush();
    expect(calls.watch).toBe(1);
    settleWatch();
    await flush();
    lease.release();
    await flush();
    expect(calls.release).toBe(1);
  });

  it("delivers a refresh published before the watch response arrived", async () => {
    const { client, publish } = stubClient();
    let settleWatch!: () => void;
    vi.mocked(client.watch).mockImplementation(() => new Promise((resolve) => {
      settleWatch = () => resolve({
        watchId: "watch", rootToken: root.token, connectionEpoch: scope.terminalEpoch,
        status: snapshot("1"), release: () => undefined,
      });
    }));
    const store = new GitRepositoryStore(client);
    const lease = store.acquire(scope, root);
    await flush();

    // The host registers a subscription before it reads the bootstrap status,
    // so a change can be published while the response is still in flight. It is
    // held until the bootstrap lands and applied after it — losing it would
    // leave the panel showing a repository state that has already moved.
    publish(snapshot("4"));
    await flush();
    expect(lease.handle.state().status).toBeUndefined();
    settleWatch();
    await flush();
    expect(lease.handle.state().status?.generation).toBe("4");
    expect(lease.handle.state().loading).toBe(false);
    lease.release();
  });

  it("ignores events belonging to another watch or another root", async () => {
    const { client, emit } = stubClient();
    const store = new GitRepositoryStore(client);
    const handle = store.acquire(scope, root);
    await flush();
    emit({ kind: "status", rootToken: "other-root", watchId: "watch", status: snapshot("9") });
    emit({ kind: "status", rootToken: root.token, watchId: "someone-else", status: snapshot("9") });
    await flush();
    expect(handle.handle.state().status?.generation).toBe("1");
    handle.release();
  });
});

const stageRequest = {
  kind: "stageFile" as const, path: "YQ==", target: "unstaged" as const,
  expectedStatusGeneration: "1", expectedSourceGeneration: "source-1",
};

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
