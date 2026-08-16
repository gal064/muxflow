// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceClient, FileWorkspaceScope, WorkspaceEvent } from "./types";
import { keyForTransferConnection } from "./api";
import { useWorkspaceFiles } from "./useWorkspaceFiles";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** The hook's own constants, so the test moves with them rather than guessing. */
const DIRECTORY_REFRESH_COALESCE_MS = 150;
const ACTIVE_ROOT_BACKSTOP_MS = 15_000;

/**
 * Fake-timer clock that remembers where it is, so a test can place an event
 * relative to the root backstop's phase rather than guessing at it.
 */
function fakeClock() {
  let elapsed = 0;
  const advance = async (ms: number) => {
    elapsed += ms;
    await vi.advanceTimersByTimeAsync(ms);
    await Promise.resolve();
  };
  return {
    advance,
    /** Runs out the coalescing window and lets React catch up. */
    settle: () => advance(DIRECTORY_REFRESH_COALESCE_MS + 10),
    /** Stops `lead` ms short of the next active-root backstop probe. */
    justBeforePoll: (lead: number) => advance(ACTIVE_ROOT_BACKSTOP_MS - (elapsed % ACTIVE_ROOT_BACKSTOP_MS) - lead),
  };
}

const BASE_SCOPE = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1", paneId: "%1" } as const;

function entry(path: string, options: { directory?: boolean; generation?: string } = {}) {
  return {
    path,
    name: path.split("/").at(-1) ?? path,
    kind: (options.directory ? "directory" : "file") as "directory" | "file",
    sizeBytes: "1",
    modifiedMillis: "1",
    generation: options.generation ?? "1",
    executable: false,
    expandable: Boolean(options.directory),
  };
}

function listing(rootToken: string, directory: string, entries: ReturnType<typeof entry>[] = []) {
  return { rootToken, directory, revision: "1", entries, overflowRecovery: false, complete: true };
}

/**
 * A client whose watch bootstrap is the directory's listing, as the production
 * one's is, so a test can count exactly how many remote reads an interaction
 * costs.
 */
function watchingClient(directories: Map<string, ReturnType<typeof entry>[]>, root: ActiveRoot) {
  const acquired: string[] = [];
  const released: string[] = [];
  const listed: string[] = [];
  let listener: ((event: WorkspaceEvent) => void) | undefined;
  const client: FileWorkspaceClient = {
    resolveActiveRoot: vi.fn(async () => root),
    listDirectory: vi.fn(async (_scope, active, directory) => {
      listed.push(directory);
      return listing(active.token, directory, directories.get(directory) ?? []);
    }),
    acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => {
      acquired.push(directory);
      return {
        snapshot: listing(active.token, directory, directories.get(directory) ?? []),
        release: () => released.push(directory),
      };
    }),
    openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
    subscribe: vi.fn(async (_scope, next) => { listener = next; return () => { listener = undefined; }; }),
  };
  return { acquired, client, listed, released, publish: (event: WorkspaceEvent) => listener?.(event) };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("useWorkspaceFiles", () => {
  it("rejects a late root from the previously active pane while existing callers can retain their root token", async () => {
    const first = deferred<ActiveRoot>();
    const second = deferred<ActiveRoot>();
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn((scope) => scope.paneId === "%1" ? first.promise : second.promise),
      listDirectory: vi.fn(async (_scope, root, directory) => ({ rootToken: root.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: { rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true }, release: () => undefined })), openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    const base = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1" };
    const one: FileWorkspaceScope = { ...base, paneId: "%1" };
    const two: FileWorkspaceScope = { ...base, paneId: "%2" };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={one} />); });
    await act(async () => { renderer.update(<Harness scope={two} />); });
    const rootTwo = { token: "two", paneId: "%2", cwd: "/two", path: "/two", gitWorktree: false, revision: "2" };
    await act(async () => { second.resolve(rootTwo); await Promise.resolve(); });
    expect(current?.root).toEqual(rootTwo);
    await act(async () => { first.resolve({ token: "one", paneId: "%1", cwd: "/one", path: "/one", gitWorktree: false, revision: "1" }); await Promise.resolve(); });
    expect(current?.root).toEqual(rootTwo);
    await act(async () => { renderer.unmount(); await Promise.resolve(); });
  });

  it("expands, revisits, and collapses through watch leases alone, never a second list", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([
      ["/repo", [entry("/repo/src", { directory: true }), entry("/repo/a.txt")]],
      ["/repo/src", [entry("/repo/src/main.ts")]],
    ]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    // The root's listing arrived with its watch: one round trip, not two.
    expect(fixture.acquired).toEqual(["/repo"]);
    expect(fixture.listed).toEqual([]);
    expect(current?.listings.get("/repo")?.entries).toHaveLength(2);

    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.acquired, "expanding one folder acquired exactly one watch").toEqual(["/repo", "/repo/src"]);
    expect(fixture.listed, "expansion paid a redundant directory list").toEqual([]);
    expect(current?.listings.get("/repo/src")?.entries).toHaveLength(1);

    // Collapsing releases that one watch and touches nothing else.
    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    expect(fixture.released).toEqual(["/repo/src"]);
    expect(fixture.acquired).toEqual(["/repo", "/repo/src"]);

    // Revisiting paints from the cache on the spot, then revalidates.
    await act(async () => { current?.toggleDirectory("/repo/src"); });
    expect(current?.listings.get("/repo/src")?.entries, "a cached revisit did not paint locally").toHaveLength(1);
    expect(current?.loading.has("/repo/src")).toBe(false);
    await act(async () => { await Promise.resolve(); });
    expect(fixture.acquired).toEqual(["/repo", "/repo/src", "/repo/src"]);
    expect(fixture.listed).toEqual([]);
    await act(async () => { renderer.unmount(); });
  });

  it("patches a listing from a precise event and lists only when it genuinely cannot", async () => {
    vi.useFakeTimers();
    const clock = fakeClock();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    // A created file is one row, and costs no request at all.
    await act(async () => {
      fixture.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/new.txt", generation: "5", entry: entry("/repo/new.txt", { generation: "5" }) });
      await clock.settle();
    });
    expect(fixture.listed, "a precise create re-listed the whole directory").toEqual([]);
    expect(current?.listings.get("/repo")?.entries.map((item) => item.path)).toEqual(["/repo/a.txt", "/repo/new.txt"]);

    // So is an edit to a row already present.
    await act(async () => {
      fixture.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/a.txt", generation: "9", entry: entry("/repo/a.txt", { generation: "9" }) });
      await clock.settle();
    });
    expect(fixture.listed).toEqual([]);
    expect(current?.listings.get("/repo")?.entries.find((item) => item.path === "/repo/a.txt")?.generation).toBe("9");

    // And a delete.
    await act(async () => {
      fixture.publish({ kind: "fileDeleted", rootToken: "root", path: "/repo/new.txt" });
      await clock.settle();
    });
    expect(fixture.listed).toEqual([]);
    expect(current?.listings.get("/repo")?.entries.map((item) => item.path)).toEqual(["/repo/a.txt"]);

    // An event the host could not map is the one case that owes a recovery
    // list, and a burst of them still owes exactly one.
    // A recovery is scheduled from an effect, so the burst is delivered first
    // and the coalescing window is run out afterwards.
    await act(async () => {
      for (let index = 0; index < 5; index += 1) {
        fixture.publish({ kind: "fileChanged", rootToken: "root", path: `/repo/opaque-${index}`, generation: "1" });
      }
    });
    await act(async () => { await clock.settle(); });
    expect(fixture.listed, "a burst of unmappable events owed more than one list").toEqual(["/repo"]);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  it("keeps every row when precise events for one directory arrive in one batch", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    // One synchronous batch, which is exactly how the host frame reader
    // delivers a burst. Each patch must build on the one before it.
    await act(async () => {
      for (const name of ["b", "c", "d"]) {
        fixture.publish({
          kind: "fileChanged", rootToken: "root", path: `/repo/${name}.txt`,
          generation: "2", entry: entry(`/repo/${name}.txt`, { generation: "2" }),
        });
      }
    });
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name))
      .toEqual(["a.txt", "b.txt", "c.txt", "d.txt"]);
    expect(fixture.listed, "a batch of patchable events still owed a list").toEqual([]);

    // And a batched delete of one of them takes exactly that row.
    await act(async () => {
      fixture.publish({ kind: "fileDeleted", rootToken: "root", path: "/repo/b.txt" });
      fixture.publish({ kind: "fileDeleted", rootToken: "root", path: "/repo/c.txt" });
    });
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["a.txt", "d.txt"]);
    await act(async () => { renderer.unmount(); });
  });

  it("re-arms its watches when the same path becomes a different root", async () => {
    vi.useFakeTimers();
    let active: ActiveRoot = { token: "first", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const acquired: Array<{ token: string; directory: string }> = [];
    const released: string[] = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => active),
      listDirectory: vi.fn(async (_scope, root, directory) => listing(root.token, directory, [entry(`${directory}/after`)])),
      acquireDirectoryWatch: vi.fn(async (_scope, root, directory) => {
        acquired.push({ token: root.token, directory });
        return {
          snapshot: listing(root.token, directory, [entry(`${directory}/${root.token}`)]),
          release: () => released.push(`${root.token}:${directory}`),
        };
      }),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["first"]);

    // The directory was replaced in place: same path, new capability. The old
    // watches must be released and new ones armed, or the tree keeps a listing
    // for a root that no longer exists and never hears about it again.
    active = { token: "second", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "2" };
    await act(async () => { await vi.advanceTimersByTimeAsync(ACTIVE_ROOT_BACKSTOP_MS + 10); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.root?.token).toBe("second");
    expect(released).toEqual(["first:/repo"]);
    expect(acquired).toEqual([{ token: "first", directory: "/repo" }, { token: "second", directory: "/repo" }]);
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["second"]);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  it("lists a directory whose watch the host refused, instead of showing nothing", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listDirectory = vi.fn(async (_scope: FileWorkspaceScope, active: ActiveRoot, directory: string) =>
      listing(active.token, directory, [entry(`${directory}/listed`)]));
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory,
      acquireDirectoryWatch: vi.fn(async () => { throw new Error("watch limit of 128 directories reached"); }),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    for (let turn = 0; turn < 4; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(listDirectory).toHaveBeenCalledWith(expect.anything(), root, "/repo", expect.anything());
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["listed"]);
    expect(current?.error, "a directory the app could still list must not show an error").toBeUndefined();
    await act(async () => { renderer.unmount(); });
  });

  it("restores the pages an authoritative first-page rescan would have deleted", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const page = (names: string[], complete: boolean) => ({
      ...listing("root", "/repo", names.map((name) => entry(`/repo/${name}`))),
      complete,
      ...(complete ? {} : { nextPageToken: "next" }),
    });
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async () => page(["c", "d"], true)),
      acquireDirectoryWatch: vi.fn(async () => ({ snapshot: page(["a", "b"], false), release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { published = next; return () => undefined; }),
    };
    let published: ((event: WorkspaceEvent) => void) | undefined;
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    // The user pages to the end.
    await act(async () => { current?.loadMore("/repo"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries).toHaveLength(4);

    // An authoritative rescan carries only page one. The rows the user can see
    // must not disappear because the host answered a smaller question.
    await act(async () => { published?.({ kind: "directorySnapshot", rootToken: "root", listing: page(["a", "b"], false) }); });
    for (let turn = 0; turn < 4; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries).toHaveLength(4);
    await act(async () => { renderer.unmount(); });
  });

  it("replaces a listing from an authoritative rescan without asking for it again", async () => {
    vi.useFakeTimers();
    const clock = fakeClock();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    await act(async () => {
      fixture.publish({
        kind: "directorySnapshot",
        rootToken: "root",
        listing: { ...listing("root", "/repo", [entry("/repo/rebuilt")]), overflowRecovery: true },
      });
      await clock.settle();
    });
    expect(fixture.listed, "an authoritative snapshot was answered with another list").toEqual([]);
    expect(current?.listings.get("/repo")?.entries.map((item) => item.path)).toEqual(["/repo/rebuilt"]);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  it("never reads a path from the root it was raised under against the root that replaced it", async () => {
    vi.useFakeTimers();
    const clock = fakeClock();
    let activeRoot: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listDirectory = vi.fn(async (_scope: FileWorkspaceScope, active: ActiveRoot, directory: string) => listing(active.token, directory));
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => activeRoot),
      listDirectory,
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: listing(active.token, directory), release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    // Sit just short of the backstop, raise an unmappable event so a recovery
    // list is deferred, then let the root move inside that window.
    await act(async () => { await clock.justBeforePoll(20); });
    activeRoot = { token: "next", paneId: "%1", cwd: "/other", path: "/other", gitWorktree: true, revision: "2" };
    await act(async () => {
      listener?.({ kind: "fileChanged", rootToken: "root", path: "/repo/racing.txt", generation: "4" });
      await clock.advance(25);
    });
    expect(current?.root?.token, "the fixture never moved the root inside the window").toBe("next");
    await act(async () => { await clock.settle(); });
    expect(
      listDirectory.mock.calls.some((call) => call[2] === "/repo"),
      "a path from the previous root was listed against the root that replaced it",
    ).toBe(false);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  /**
   * The Phase 14 wide Explorer lane, as a request ledger rather than a timing.
   *
   * 4,096 entries is the size at which a list-per-event or a watch rebuild is
   * unmistakable, so the counts here are the evidence that expanding, changing,
   * revisiting, and collapsing cost exactly the round trips they should.
   */
  it("holds the Phase 14 wide Explorer lane to one watch per directory and no list at all", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const wide = Array.from({ length: 4_096 }, (_, index) => entry(`/repo/wide/file-${index}`));
    const directories = new Map([
      ["/repo", [entry("/repo/wide", { directory: true })]],
      ["/repo/wide", wide],
    ]);
    const fixture = watchingClient(directories, root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    const rootWatches = fixture.acquired.length;

    await act(async () => { current?.toggleDirectory("/repo/wide"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    const expandWatches = fixture.acquired.length - rootWatches;
    const expandedRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;

    // One external create inside the 4,096-entry directory. The fake host
    // learns about it too, so a later revalidation agrees with the patch
    // rather than silently undoing it.
    directories.set("/repo/wide", [...wide, entry("/repo/wide/appeared", { generation: "2" })]);
    await act(async () => {
      fixture.publish({
        kind: "fileChanged", rootToken: "root", path: "/repo/wide/appeared",
        generation: "2", entry: entry("/repo/wide/appeared", { generation: "2" }),
      });
      await Promise.resolve();
    });
    const afterChangeRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;

    await act(async () => { current?.toggleDirectory("/repo/wide"); await Promise.resolve(); });
    const collapseReleases = fixture.released.length;
    await act(async () => { current?.toggleDirectory("/repo/wide"); });
    const cachedRevisitRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;
    await act(async () => { await Promise.resolve(); });

    expect(rootWatches).toBe(1);
    expect(expandWatches).toBe(1);
    expect(expandedRows).toBe(4_096);
    expect(afterChangeRows).toBe(4_097);
    expect(collapseReleases).toBe(1);
    expect(cachedRevisitRows).toBe(4_097);
    expect(fixture.listed).toEqual([]);
    console.log(`PHASE14_METRIC ${JSON.stringify({
      lane: "explorerWideWatchTraffic",
      entries: 4_096,
      rootWatchRequests: rootWatches,
      expandWatchRequests: expandWatches,
      directoryListRequests: fixture.listed.length,
      collapseWatchReleases: collapseReleases,
      expandedRows,
      externalChangeRows: afterChangeRows,
      cachedRevisitRows,
      cachedRevisitPaintedBeforeRevalidation: cachedRevisitRows === afterChangeRows,
    })}`);
    await act(async () => { renderer.unmount(); });
  });

  it("retains an ongoing connection-owned download across pane/session/root switch through published completion", async () => {
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listeners: Array<(event: WorkspaceEvent) => void> = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (active) => ({ ...root, paneId: active.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: { rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true }, release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, listener) => { listeners.push(listener); return () => undefined; }),
    };
    const base = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1" };
    const one: FileWorkspaceScope = { ...base, paneId: "%1" };
    const two: FileWorkspaceScope = { ...base, sessionId: "$2", paneId: "%2" };
    const transferScopeKey = keyForTransferConnection(one);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={one} />); await Promise.resolve(); });
    await act(async () => {
      listeners.forEach((listener) => listener({ kind: "transfer", transfer: {
        id: "download", scopeKey: transferScopeKey, path: "/repo/a", kind: "file", state: "running",
        completedBytes: "2", filesCompleted: "0",
      } }));
    });
    expect(current?.transfers[0]?.state).toBe("running");
    await act(async () => { renderer.update(<Harness scope={two} />); await Promise.resolve(); });
    expect(current?.root?.paneId).toBe("%2");
    expect(current?.transfers[0]).toMatchObject({ id: "download", state: "running" });
    await act(async () => {
      listeners.forEach((listener) => listener({ kind: "transfer", transfer: {
        id: "download", scopeKey: transferScopeKey, path: "/repo/a", kind: "file", state: "completed", outcome: "published",
        completedBytes: "3", filesCompleted: "1",
      } }));
    });
    expect(current?.transfers[0]).toMatchObject({ state: "completed", outcome: "published", completedBytes: "3" });
    await act(async () => { renderer.unmount(); });
  });

  it("terminalizes a live row only when its owning connection is replaced", async () => {
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (active) => ({ ...root, paneId: active.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: { rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true }, release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    const one: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1", paneId: "%1" };
    const two: FileWorkspaceScope = { ...one, serverIdentity: "new", terminalEpoch: 42 };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={one} />); await Promise.resolve(); });
    await act(async () => { listener?.({ kind: "transfer", transfer: {
      id: "download", scopeKey: keyForTransferConnection(one), path: "/repo/a", kind: "file", state: "verifying",
      completedBytes: "2", filesCompleted: "0",
    } }); });
    await act(async () => { renderer.update(<Harness scope={two} />); await Promise.resolve(); });
    expect(current?.transfers[0]).toMatchObject({ state: "failed", outcome: "unknown", failureKind: "staleScope" });
    await act(async () => { renderer.unmount(); });
  });

  it("never downgrades a download quarantine cleanup failure after a later no-op event", async () => {
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: { rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true }, release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1", paneId: "%1" };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    const base = { id: "download", scopeKey: keyForTransferConnection(scope), path: "/repo/a", kind: "file" as const, completedBytes: "12", filesCompleted: "0" };
    await act(async () => { listener?.({ kind: "transfer", transfer: {
      ...base, state: "failed", outcome: "notPublished", failureKind: "transfer", cleanupStatus: "retained", cleanupError: "partial quarantined",
    } }); });
    await act(async () => { listener?.({ kind: "transfer", transfer: {
      ...base, state: "cancelled", outcome: "notPublished", cleanupStatus: "removed", error: "cancel was already terminal",
    } }); });
    expect(current?.transfers[0]).toMatchObject({
      state: "failed", failureKind: "transfer", cleanupStatus: "retained", cleanupError: "partial quarantined",
    });
    expect(current?.transfers[0]?.error).not.toBe("cancel was already terminal");
    await act(async () => { renderer.unmount(); });
  });
});
