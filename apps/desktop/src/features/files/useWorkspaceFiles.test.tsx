// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceClient, FileWorkspaceScope, WorkspaceEvent } from "./types";
import { keyForScope, keyForTransferConnection } from "./api";
import {
  armPanePaint,
  notePanePainted,
  PANE_PAINT_TIMEOUT_MS,
  resetPanePaintGate,
} from "../terminal/panePaintGate";
import { enablePerfProbe, perfSummary, resetPerfProbe } from "../../perf/probe";
import { ACTIVE_ROOT_SETTLED_MULTIPLIER, ACTIVE_ROOT_STABLE_PROBES, useWorkspaceFiles } from "./useWorkspaceFiles";

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
  return { rootToken, directory, revision: "1", entries, recoveredFromOverflow: false, complete: true };
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
        fresh: true,
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

// The paint gate is a module singleton, so a pane one test armed would gate
// the next one's probe. Every test that does not arm it wants the ordinary
// case: nothing pending, nothing waited for.
afterEach(() => { resetPanePaintGate(); });

describe("useWorkspaceFiles", () => {
  it("keeps the authoritative tree through a transport gap and revalidates it once connected", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    const selectionKey = "local\0s\0$1\0%1";
    const replacement: FileWorkspaceScope = { ...BASE_SCOPE, clientId: "next", terminalEpoch: 42 };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope?: FileWorkspaceScope }) {
      current = useWorkspaceFiles(fixture.client, scope, selectionKey);
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={BASE_SCOPE} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.root).toEqual(root);
    expect(current?.listings.get("/repo")?.entries).toHaveLength(1);

    await act(async () => { renderer.update(<Harness />); await Promise.resolve(); });
    expect(current?.root, "the reconnect blanked the root").toEqual(root);
    expect(current?.listings.get("/repo")?.entries, "the reconnect blanked the tree").toHaveLength(1);
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(1);

    await act(async () => { renderer.update(<Harness scope={replacement} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(2);
    expect(fixture.client.resolveActiveRoot).toHaveBeenLastCalledWith(replacement, { knownRootToken: "root" });
    expect(current?.listings.get("/repo")?.entries).toHaveLength(1);
    expect(fixture.acquired).toEqual(["/repo", "/repo"]);
    await act(async () => { renderer.unmount(); });
  });

  it("rejects a late root from the previously active pane while existing callers can retain their root token", async () => {
    const first = deferred<ActiveRoot>();
    const second = deferred<ActiveRoot>();
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn((scope) => scope.paneId === "%1" ? first.promise : second.promise),
      listDirectory: vi.fn(async (_scope, root, directory) => ({ rootToken: root.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ fresh: true, snapshot: { rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true }, release: () => undefined })), openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
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

    // Re-expanding without leaving keeps the rows it already had.
    await act(async () => { current?.toggleDirectory("/repo/src"); });
    expect(current?.listings.get("/repo/src")?.entries).toHaveLength(1);
    expect(current?.loading.has("/repo/src")).toBe(false);
    await act(async () => { await Promise.resolve(); });
    expect(fixture.acquired).toEqual(["/repo", "/repo/src", "/repo/src"]);
    expect(fixture.listed).toEqual([]);
    await act(async () => { renderer.unmount(); });
  });

  /**
   * The pane-switch fixture the three staleness lanes share.
   *
   * The second pane's root resolution is deliberately held open, because the
   * window between "the selection moved" and "the new root landed" is the
   * whole subject: it is what the Explorer used to spend showing an empty
   * tree, and what `stale` now names.
   */
  function paneSwitchFixture(second: Promise<ActiveRoot>) {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const directories = new Map([
      ["/repo", [entry("/repo/src", { directory: true })]],
      ["/repo/src", [entry("/repo/src/main.ts"), entry("/repo/src/util.ts")]],
    ]);
    const acquired: string[] = [];
    const released: string[] = [];
    const listed: string[] = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn((scope) => scope.paneId === "%1" ? Promise.resolve(root) : second),
      listDirectory: vi.fn(async (_scope, active, directory) => {
        listed.push(directory);
        return listing(active.token, directory, directories.get(directory) ?? []);
      }),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => {
        acquired.push(directory);
        return {
          fresh: true,
          snapshot: listing(active.token, directory, directories.get(directory) ?? []),
          release: () => released.push(directory),
        };
      }),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    return { acquired, client, directories, listed, released, root };
  }

  it("keeps the previous pane's tree painted across a same-root pane switch, and adopts it", async () => {
    // The dominant Explorer flicker, and the contract that replaced it. The
    // selection key carries `paneId`, so this transition used to empty the
    // whole tree and drop `root` to `undefined` — which took the Git rail to
    // IDLE with it — before repainting the identical tree a round trip later.
    const second = deferred<ActiveRoot>();
    const fixture = paneSwitchFixture(second.promise);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    const seen: Array<{ rows: number; root: boolean }> = [];
    function Harness({ scope }: { scope: FileWorkspaceScope }) {
      current = useWorkspaceFiles(fixture.client, scope);
      seen.push({ rows: current.listings.size, root: Boolean(current.root) });
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={BASE_SCOPE} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo/src")?.entries).toHaveLength(2);
    expect(current?.stale).toBe(false);

    // The switch itself. Nothing has resolved for the new pane yet.
    seen.length = 0;
    const other: FileWorkspaceScope = { ...BASE_SCOPE, paneId: "%2" };
    await act(async () => { renderer.update(<Harness scope={other} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.stale, "the changeover window was not published as stale").toBe(true);
    expect(current?.listings.get("/repo/src")?.entries, "the tree blanked on a pane switch").toHaveLength(2);
    expect(current?.root, "the root flapped, which is what blanks the Git rail").toEqual(fixture.root);

    // The same worktree under the new pane: the same capability token, and a
    // `paneId` that is the only thing about it that moved.
    await act(async () => {
      second.resolve({ ...fixture.root, paneId: "%2" });
      await Promise.resolve();
    });
    await act(async () => { await Promise.resolve(); });
    expect(current?.stale).toBe(false);
    expect(current?.root?.paneId).toBe("%2");
    expect(current?.root?.token).toBe("root");
    expect(current?.listings.get("/repo/src")?.entries, "the adopted tree was rebuilt from empty").toHaveLength(2);
    expect(current?.expanded.has("/repo/src")).toBe(true);
    expect(
      seen.every((frame) => frame.rows === 2 && frame.root),
      "the tree or the root emptied for at least one commit during the switch",
    ).toBe(true);
    expect(fixture.listed, "the switch paid for a directory list").toEqual([]);
    // The watches follow the selection rather than the paint: they are given
    // back when the pane changes and armed again against the adopted root, so
    // the tree that stayed on screen is live again once the window closes.
    expect(fixture.released).toEqual(["/repo", "/repo/src"]);
    expect(fixture.acquired).toEqual(["/repo", "/repo/src", "/repo", "/repo/src"]);
    await act(async () => { renderer.unmount(); });
  });

  it("paints the previous tree until a genuinely different root resolves, then swaps to it", async () => {
    const second = deferred<ActiveRoot>();
    const fixture = paneSwitchFixture(second.promise);
    fixture.directories.set("/elsewhere", [entry("/elsewhere/readme.md")]);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(fixture.client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={BASE_SCOPE} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries).toHaveLength(1);

    await act(async () => { renderer.update(<Harness scope={{ ...BASE_SCOPE, paneId: "%2" }} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.stale).toBe(true);
    expect(current?.listings.get("/repo")?.entries, "the tree blanked before it knew it had to").toHaveLength(1);

    // A different capability is a different worktree. Everything the previous
    // one authorised goes with it, and a brief empty tree here is correct.
    await act(async () => {
      second.resolve({ token: "elsewhere", paneId: "%2", cwd: "/elsewhere", path: "/elsewhere", gitWorktree: true, revision: "2" });
      await Promise.resolve();
    });
    expect(current?.stale).toBe(false);
    expect(current?.root?.token).toBe("elsewhere");
    expect(current?.listings.get("/repo"), "a replaced root kept the previous root's listings").toBeUndefined();
    await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/elsewhere")?.entries.map((row) => row.name)).toEqual(["readme.md"]);
    await act(async () => { renderer.unmount(); });
  });

  it("refuses every interaction while the painted tree is not the selection's own", async () => {
    // The blank used to be the safety: an Explorer with no rows could not be
    // asked to act on another pane's root. The rows are on screen now, so the
    // refusal has to be stated where the interaction enters.
    const second = deferred<ActiveRoot>();
    const fixture = paneSwitchFixture(second.promise);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(fixture.client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={BASE_SCOPE} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    await act(async () => { renderer.update(<Harness scope={{ ...BASE_SCOPE, paneId: "%2" }} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.stale).toBe(true);
    const acquiredBefore = fixture.acquired.length;

    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    await act(async () => { current?.refresh("/repo"); await Promise.resolve(); });
    await act(async () => { current?.loadMore("/repo"); await Promise.resolve(); });
    expect(current?.expanded.has("/repo/src"), "a stale expansion changed the tree").toBe(false);
    expect(fixture.listed, "a stale interaction issued a remote read").toEqual([]);
    expect(fixture.acquired.length, "a stale expansion armed a watch").toBe(acquiredBefore);
    expect(current?.requestedReads, "a stale refresh showed the user a wait it never owed them").toBe(0);
    // Refresh is not inert, though: it re-checks the root, which is the one
    // thing that ends the window a person pressing it is trying to escape.
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledWith(
      expect.objectContaining({ paneId: "%2" }),
      { knownRootToken: "root" },
    );

    await act(async () => { second.resolve({ ...fixture.root, paneId: "%2" }); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.stale).toBe(false);
    // And the same interaction, once the window has closed, is answered.
    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(current?.expanded.has("/repo/src")).toBe(true);
    expect(current?.listings.get("/repo/src")?.entries).toHaveLength(2);
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
          fresh: true,
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

  it("never shows fewer rows than it had while an authoritative rescan is being restored", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const page = (names: string[], options: { complete: boolean; revision?: string }) => ({
      ...listing("root", "/repo", names.map((name) => entry(`/repo/${name}`))),
      revision: options.revision ?? "1",
      complete: options.complete,
      ...(options.complete ? {} : { nextPageToken: "page-2" }),
    });
    let published: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async (_scope, _active, _directory, options) => options?.pageToken
        ? page(["c", "d"], { complete: true, revision: "1" })
        : page(["a", "b"], { complete: false, revision: "1" })),
      acquireDirectoryWatch: vi.fn(async () => ({ fresh: true, snapshot: page(["a", "b"], { complete: false }), release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { published = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    const seen: number[] = [];
    function Harness() {
      current = useWorkspaceFiles(client, BASE_SCOPE);
      const rows = current.listings.get("/repo")?.entries.length;
      if (rows !== undefined) seen.push(rows);
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    // The bootstrap's page one, then the bounded prefetch of page two.
    for (let turn = 0; turn < 5; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries).toHaveLength(4);

    // An authoritative rescan carries only page one. The rows the user can see
    // must not disappear, even for one commit: dropping to two rows would take
    // the keyboard focus down with them and never put it back.
    seen.length = 0;
    await act(async () => { published?.({ kind: "directorySnapshot", rootToken: "root", listing: page(["a", "b"], { complete: false, revision: "9" }) }); });
    for (let turn = 0; turn < 6; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(Math.min(...seen), "the tree shrank while its pages were being restored").toBe(4);
    expect(current?.listings.get("/repo")?.entries).toHaveLength(4);
    await act(async () => { renderer.unmount(); });
  });

  it("revalidates a bootstrap that came from a watch it did not arm", async () => {
    // One host watch serves every surface, and its bootstrap is produced once —
    // when the watch was armed. An open file's tab watches its own folder, so
    // an Explorer that expands that folder later joins an existing watch and is
    // handed a listing of arbitrarily old rows. Painting it is the point;
    // trusting it silently reverted every row created since.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listed: string[] = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async (_scope, active, directory) => {
        listed.push(directory);
        return listing(active.token, directory, [entry("/repo/created-since")]);
      }),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: false,
        snapshot: listing(active.token, directory, [entry("/repo/as-it-was")]),
        release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    for (let turn = 0; turn < 5; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(listed, "an inherited bootstrap was trusted as if it were current").toEqual(["/repo"]);
    expect(current?.listings.get("/repo")?.entries.map((row) => row.name)).toEqual(["created-since"]);
    await act(async () => { renderer.unmount(); });
  });

  it("refuses a page that continues a listing the tree no longer holds", async () => {
    // The page and the rescan race on the remote link. A page from the
    // superseded listing would put back rows the rescan removed, and roll the
    // listing's own revision backwards while doing it.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const page = (names: string[], revision: string, complete: boolean) => ({
      ...listing("root", "/repo", names.map((name) => entry(`/repo/${name}`))),
      revision,
      complete,
      ...(complete ? {} : { nextPageToken: "page-2" }),
    });
    let releasePage: (() => void) | undefined;
    let published: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async () => {
        await new Promise<void>((resolve) => { releasePage = resolve; });
        // A slice of the listing that has since been replaced.
        return page(["stale"], "1", true);
      }),
      acquireDirectoryWatch: vi.fn(async () => ({ fresh: true, snapshot: page(["a"], "1", false), release: () => undefined })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { published = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    for (let turn = 0; turn < 4; turn += 1) await act(async () => { await Promise.resolve(); });
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["a"]);

    // A complete rescan lands while the prefetched page is still in flight.
    await act(async () => { published?.({ kind: "directorySnapshot", rootToken: "root", listing: page(["a"], "9", true) }); });
    releasePage?.();
    for (let turn = 0; turn < 4; turn += 1) await act(async () => { await Promise.resolve(); });
    const held = current?.listings.get("/repo");
    expect(held?.entries.map((item) => item.name), "a stale page put rows back").toEqual(["a"]);
    expect(held?.revision, "the listing's revision went backwards").toBe("9");
    expect(held?.complete).toBe(true);
    await act(async () => { renderer.unmount(); });
  });

  /**
   * Collapsing a directory ends the remote work its rows were asking for —
   * all of it, including the read that has not gone out yet.
   *
   * A recovery list waits out a 150 ms coalescing window before it is issued,
   * and a collapse inside that window used to leave the timer armed: the list
   * went out for a directory the tree no longer showed, making "collapse is one
   * unwatch" sometimes one unwatch and one full listing. Worse, the answer then
   * arrived for a directory every state transition refuses, so the wait state
   * it had taken was never given back and `aria-busy` stayed set on the whole
   * Explorer for the life of the scope with nothing on screen to explain it.
   */
  it("collapsing a directory cancels the recovery list it had not issued yet", async () => {
    vi.useFakeTimers();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const directories = new Map([
      ["/repo", [entry("/repo/src", { directory: true })]],
      ["/repo/src", [entry("/repo/src/a.ts")]],
    ]);
    const fixture = watchingClient(directories, root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    try {
      await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
      await act(async () => { await Promise.resolve(); });
      await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
      await act(async () => { await Promise.resolve(); });
      expect(fixture.listed, "the bootstrap is the listing").toEqual([]);

      // An event this listing cannot answer: no entry, so no patch is possible
      // and a recovery list is owed.
      await act(async () => {
        fixture.publish({ kind: "fileChanged", rootToken: "root", path: "/repo/src/new.ts", generation: "2" });
        await Promise.resolve();
      });
      // Collapsed inside the coalescing window, then well past it.
      await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });

      expect(fixture.listed, "a collapsed directory was listed anyway").toEqual([]);
      expect(
        current?.loading.has("/repo/src"),
        "the collapsed directory was left waiting on a read that never lands",
      ).toBe(false);
      expect(current?.loading.size, "aria-busy would stay set on the whole Explorer").toBe(0);
    } finally {
      await act(async () => { renderer?.unmount(); });
      vi.useRealTimers();
    }
  });

  it("stops probing the active root once it has settled, and returns to full rate on activity without probing", async () => {
    vi.useFakeTimers();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const resolveActiveRoot = vi.fn(async () => root);
    const client: FileWorkspaceClient = {
      resolveActiveRoot,
      listDirectory: vi.fn(async (_scope, active, directory) => listing(active.token, directory)),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: true,
        snapshot: listing(active.token, directory, directory === "/repo" ? [entry("/repo/src", { directory: true })] : []),
        release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    const initial = resolveActiveRoot.mock.calls.length;

    // Probe until the root has proved itself unchanged, then keep waiting: an
    // idle window must ask the host far less, though never nothing at all.
    for (let tick = 0; tick < ACTIVE_ROOT_SETTLED_MULTIPLIER - 1; tick += 1) {
      await act(async () => { await vi.advanceTimersByTimeAsync(ACTIVE_ROOT_BACKSTOP_MS + 1); });
    }
    const settled = resolveActiveRoot.mock.calls.length;
    expect(settled).toBeGreaterThan(initial);
    expect(settled - initial, "a settled backstop kept polling at full rate").toBeLessThanOrEqual(ACTIVE_ROOT_STABLE_PROBES);

    // But it does not switch itself off. `cd` inside the pane the user is
    // already in is announced by nothing at all, so a backstop that stops
    // never notices it again.
    await act(async () => { await vi.advanceTimersByTimeAsync(ACTIVE_ROOT_BACKSTOP_MS + 1); });
    const idle = resolveActiveRoot.mock.calls.length;
    expect(idle, "a settled backstop stopped checking altogether").toBe(settled + 1);

    // Working in the tree is activity, not evidence. It returns the backstop to
    // full rate and issues nothing: expanding a folder says nothing about
    // whether the pane's `cd` moved, and probing here put a `resolveActiveRoot`
    // — which forks `tmux` on the host — on every expand *and* every collapse,
    // the exact interaction path this package exists to make cheap.
    await act(async () => { current?.toggleDirectory("/repo/src"); await Promise.resolve(); });
    expect(
      resolveActiveRoot.mock.calls.length,
      "expanding a folder cost a remote root probe",
    ).toBe(idle);
    await act(async () => { await vi.advanceTimersByTimeAsync(ACTIVE_ROOT_BACKSTOP_MS + 1); });
    expect(
      resolveActiveRoot.mock.calls.length,
      "activity did not take the backstop back off its settled interval",
    ).toBe(idle + 1);

    // A host root announcement *is* evidence, and still probes immediately.
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  it("checks the root the moment the host reports the active pane changed directory", async () => {
    // tmux announces no `cd`, so the settled backstop is minutes away from
    // noticing one. The host's topology reconcile does notice, and the pane's
    // `currentPath` arriving different in that snapshot is the whole signal:
    // one probe, on the transition, and nothing on the transitions that are
    // only connection churn.
    vi.useFakeTimers();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const resolveActiveRoot = vi.fn(async () => root);
    const client: FileWorkspaceClient = {
      resolveActiveRoot,
      listDirectory: vi.fn(async (_scope, active, directory) => listing(active.token, directory)),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: true, snapshot: listing(active.token, directory), release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    function Harness({ panePath }: { panePath?: string }) {
      useWorkspaceFiles(client, BASE_SCOPE, undefined, panePath);
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness panePath="/repo" />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    // The scope's own probe already ran; a pane path present from the first
    // render is not a change and must not buy a second one.
    expect(resolveActiveRoot.mock.calls.length, "mount probed the root twice").toBe(1);

    for (let tick = 0; tick < ACTIVE_ROOT_SETTLED_MULTIPLIER - 1; tick += 1) {
      await act(async () => { await vi.advanceTimersByTimeAsync(ACTIVE_ROOT_BACKSTOP_MS + 1); });
    }
    const settled = resolveActiveRoot.mock.calls.length;
    expect(settled).toBeGreaterThan(1);

    // The same path, re-reported by every later snapshot, says nothing new.
    await act(async () => { renderer.update(<Harness panePath="/repo" />); await Promise.resolve(); });
    expect(resolveActiveRoot.mock.calls.length, "an unchanged pane path cost a probe").toBe(settled);

    // Losing and regaining the pane is a reconnect, not a `cd`.
    await act(async () => { renderer.update(<Harness />); await Promise.resolve(); });
    await act(async () => { renderer.update(<Harness panePath="/repo" />); await Promise.resolve(); });
    expect(
      resolveActiveRoot.mock.calls.length,
      "connection churn through `undefined` probed the root",
    ).toBe(settled);

    // A genuinely different directory probes at once, settled or not — no
    // timer advanced here on purpose.
    await act(async () => { renderer.update(<Harness panePath="/elsewhere" />); await Promise.resolve(); });
    expect(
      resolveActiveRoot.mock.calls.length,
      "the host's `cd` signal waited for the settled backstop",
    ).toBe(settled + 1);

    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });

  it("treats a host root announcement as a reason to check, never as the answer", async () => {
    // The broadcast carries no caller epoch, so installing its root would
    // reintroduce the stale cross-pane race the probe barriers exist to stop.
    // Discarding it outright was the other extreme: the one signal the host
    // can push about a moved root reached nobody.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const resolveActiveRoot = vi.fn(async () => root);
    let published: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot,
      listDirectory: vi.fn(async (_scope, active, directory) => listing(active.token, directory)),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: true, snapshot: listing(active.token, directory), release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { published = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    for (let turn = 0; turn < 3; turn += 1) await act(async () => { await Promise.resolve(); });
    const probes = resolveActiveRoot.mock.calls.length;

    const announced: ActiveRoot = { token: "other", paneId: "%1", cwd: "/elsewhere", path: "/elsewhere", gitWorktree: false, revision: "4" };
    await act(async () => { published?.({ kind: "rootChanged", root: announced }); await Promise.resolve(); });
    expect(resolveActiveRoot.mock.calls.length, "the announcement reached nobody").toBeGreaterThan(probes);
    expect(current?.root, "an unguarded broadcast was installed as the root").toEqual(root);
    await act(async () => { renderer.unmount(); });
  });

  it("prefetches exactly one further page for a directory that opened incomplete", async () => {
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listed: string[] = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => root),
      listDirectory: vi.fn(async (_scope, active, directory) => {
        listed.push(directory);
        // The prefetched page finishes the directory, so nothing follows it.
        return { ...listing(active.token, directory, [entry("/repo/b")]), complete: true };
      }),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: true,
        snapshot: { ...listing(active.token, directory, [entry("/repo/a")]), complete: false, nextPageToken: "page-2" },
        release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async () => () => undefined),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    for (let turn = 0; turn < 5; turn += 1) await act(async () => { await Promise.resolve(); });
    // One page, never a chain of them.
    expect(listed).toEqual(["/repo"]);
    expect(current?.listings.get("/repo")?.entries.map((item) => item.name)).toEqual(["a", "b"]);
    expect(current?.listings.get("/repo")?.complete).toBe(true);
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
        listing: { ...listing("root", "/repo", [entry("/repo/rebuilt")]), recoveredFromOverflow: true },
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
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ fresh: true, snapshot: listing(active.token, directory), release: () => undefined })),
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
   * revisiting, and collapsing cost exactly the round trips they should. The
   * revisit deliberately crosses a pane switch, which is the transition the
   * whole lane is most sensitive to: it changes the scope key, releases every
   * watch, and re-arms them against the adopted root.
   */
  it("holds the Phase 14 wide Explorer lane to one watch per directory and no list at all", async () => {
    // The paint spans this feature is budgeted on are published from here, and
    // until now nothing read them at all. A jsdom commit is not a browser
    // paint, so the durations are recorded and deliberately not asserted; what
    // *is* asserted is that both spans are published, because a budget nothing
    // emits a sample for cannot be checked anywhere.
    enablePerfProbe(async () => undefined);
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const wide = Array.from({ length: 4_096 }, (_, index) => entry(`/repo/wide/file-${index}`));
    const directories = new Map([
      ["/repo", [entry("/repo/wide", { directory: true })]],
      ["/repo/wide", wide],
    ]);
    const acquired: string[] = [];
    const released: string[] = [];
    const listed: string[] = [];
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    let holdRevisit: (() => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (scope) => ({ ...root, paneId: scope.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory) => {
        listed.push(directory);
        return listing(active.token, directory, directories.get(directory) ?? []);
      }),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => {
        acquired.push(directory);
        // The revisit's watch is held open, so anything painted before it
        // answers came from the cache and from nothing else.
        if (directory === "/repo/wide" && acquired.filter((held) => held === directory).length > 1) {
          await new Promise<void>((resolve) => { holdRevisit = resolve; });
        }
        return {
          fresh: true,
          snapshot: listing(active.token, directory, directories.get(directory) ?? []),
          release: () => released.push(directory),
        };
      }),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness({ scope }: { scope: FileWorkspaceScope }) { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness scope={BASE_SCOPE} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    const rootWatches = acquired.length;

    await act(async () => { current?.toggleDirectory("/repo/wide"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    const expandWatches = acquired.length - rootWatches;
    const expandedRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;

    // One external create inside the 4,096-entry directory. The fake host
    // learns about it too, so a later revalidation agrees with the patch
    // rather than silently undoing it.
    directories.set("/repo/wide", [...wide, entry("/repo/wide/appeared", { generation: "2" })]);
    await act(async () => {
      listener?.({
        kind: "fileChanged", rootToken: "root", path: "/repo/wide/appeared",
        generation: "2", entry: entry("/repo/wide/appeared", { generation: "2" }),
      });
      await Promise.resolve();
    });
    const afterChangeRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;
    // Named, not just counted: 4,097 rows proves a row arrived, not that it is
    // the one the event described.
    const createdRowPresent = (current?.listings.get("/repo/wide")?.entries ?? [])
      .some((row) => row.path === "/repo/wide/appeared");
    // Read here, before the pane switch below: a paint measurement belongs to
    // the lifecycle that raised it, and switching panes abandons it — which is
    // the correct behaviour and would otherwise look like a missing span.
    // Paint spans close two animation frames after the commit that satisfied
    // them, which in jsdom is two timer ticks rather than two frames.
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 60)); });
    const spans = new Map(perfSummary().map((row) => [row.name, row]));
    const expandPaint = spans.get("workflow.explorer.directoryExpandPaint");
    const changePaint = spans.get("explorer.externalChangeToPaint");

    await act(async () => { current?.toggleDirectory("/repo/wide"); await Promise.resolve(); });
    const collapseReleases = released.filter((directory) => directory === "/repo/wide").length;

    // A pane switch across the same root capability. The lane's field names
    // predate the change that made this an *adoption* rather than a cache
    // paint — the tree is no longer dropped here at all, so the listings the
    // revisit paints from are its own — and are kept because what they assert
    // is unchanged and still the point: the revisit shows its rows before any
    // revalidation of them lands, and pays no list for the privilege.
    await act(async () => { renderer.update(<Harness scope={{ ...BASE_SCOPE, paneId: "%2" }} />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    await act(async () => { current?.toggleDirectory("/repo/wide"); });
    const cachedRevisitRows = current?.listings.get("/repo/wide")?.entries.length ?? 0;
    const cachedRevisitWaiting = current?.loading.has("/repo/wide") ?? true;
    holdRevisit?.();
    await act(async () => { await Promise.resolve(); });

    expect(rootWatches).toBe(1);
    expect(expandWatches).toBe(1);
    expect(expandedRows).toBe(4_096);
    expect(afterChangeRows).toBe(4_097);
    expect(createdRowPresent, "the extra row was not the one the event named").toBe(true);
    expect(collapseReleases).toBe(1);
    expect(cachedRevisitRows).toBe(4_097);
    expect(cachedRevisitWaiting).toBe(false);
    expect(listed).toEqual([]);
    expect(expandPaint?.n, "the expand-to-paint span was never published").toBeGreaterThan(0);
    expect(changePaint?.n, "the external-change-to-paint span was never published").toBeGreaterThan(0);
    console.log(`PHASE14_METRIC ${JSON.stringify({
      lane: "explorerWideWatchTraffic",
      jsdomExpandToPaintP95Ms: expandPaint?.p95Ms,
      jsdomExternalChangeToPaintP95Ms: changePaint?.p95Ms,
      entries: 4_096,
      rootWatchRequests: rootWatches,
      expandWatchRequests: expandWatches,
      directoryListRequests: listed.length,
      collapseWatchReleases: collapseReleases,
      expandedRows,
      externalChangeRows: afterChangeRows,
      externalChangeRowPath: createdRowPresent ? "/repo/wide/appeared" : null,
      cachedRevisitRows,
      cachedRevisitPaintedBeforeRevalidation: cachedRevisitRows === afterChangeRows && !cachedRevisitWaiting,
    })}`);
    await act(async () => { renderer.unmount(); });
    resetPerfProbe();
  });

  it("holds the Phase 14 paginated Explorer lane to one bootstrap, one prefetch, and no list per change", async () => {
    // The size class the 4,096-entry lane cannot see. The host's page is 4,096
    // entries, so a larger directory arrives incomplete and stays incomplete —
    // and the rule that decided whether a precise change could be applied
    // locally used to require a *complete* listing. Every single-file change
    // in a directory of this size therefore cost a recovery list and then a
    // full re-pagination: the exact list storm this package exists to remove,
    // in the directories where it costs most.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const PAGE = 4_096;
    // Three pages, not two. At two the prefetched second page finishes the
    // directory, `appendPage` carries its `complete: true` onto the assembled
    // listing, and every subsequent patch takes `covers()`'s complete branch —
    // the same branch the 4,096-entry lane takes. The lane then measured a
    // size and nothing else. A third page keeps the held listing genuinely
    // incomplete, which is the state this lane exists to exercise: the rule
    // that used to require a *complete* listing before it would patch.
    const all = Array.from({ length: 3 * PAGE }, (_, index) => entry(`/repo/wide/file-${String(index).padStart(5, "0")}`));
    const pageOf = (token: string | undefined) => {
      const start = token === undefined ? 0 : Number(token);
      const slice = all.slice(start, start + PAGE);
      const next = start + PAGE;
      return {
        rootToken: "root", directory: "/repo/wide", revision: "7", entries: slice,
        recoveredFromOverflow: false,
        complete: next >= all.length,
        ...(next < all.length ? { nextPageToken: String(next) } : {}),
      };
    };
    const listed: string[] = [];
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (scope) => ({ ...root, paneId: scope.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory, options) => {
        listed.push(`${directory}${options?.pageToken ? `@${options.pageToken}` : ""}`);
        if (directory !== "/repo/wide") return listing(active.token, directory, []);
        return pageOf(options?.pageToken);
      }),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({
        fresh: true,
        snapshot: directory === "/repo/wide"
          ? pageOf(undefined)
          : listing(active.token, directory, [entry("/repo/wide", { directory: true })]),
        release: () => undefined,
      })),
      openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, BASE_SCOPE); return null; }
    let renderer!: ReturnType<typeof create>;
    vi.useFakeTimers();
    try {
      await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
      await act(async () => { await Promise.resolve(); });
      await act(async () => { current?.toggleDirectory("/repo/wide"); await Promise.resolve(); });
      for (let turn = 0; turn < 4; turn += 1) await act(async () => { await Promise.resolve(); });
      // The bootstrap is page one; exactly one further page is prefetched.
      const afterExpand = listed.length;
      const expanded = current?.listings.get("/repo/wide");
      const expandedRows = expanded?.entries.length ?? 0;
      // The crux of the lane: the tree is holding two pages of a directory it
      // has not finished reading.
      const heldListingComplete = Boolean(expanded?.complete && !expanded.nextPageToken);

      // One external create, inside the rows the tree is holding.
      const created = entry("/repo/wide/file-00000a", { generation: "2" });
      await act(async () => {
        listener?.({ kind: "fileChanged", rootToken: "root", path: created.path, generation: "2", entry: created });
        await Promise.resolve();
      });
      // And one external delete, of a row it is holding.
      await act(async () => {
        listener?.({ kind: "fileDeleted", rootToken: "root", path: "/repo/wide/file-00001" });
        await Promise.resolve();
      });
      // Well past any recovery window: a list scheduled by either event would
      // have gone out by now.
      await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
      const changeLists = listed.length - afterExpand;
      const heldListing = current?.listings.get("/repo/wide");
      const held = heldListing?.entries ?? [];
      const afterChangeRows = held.length;
      // The row count alone cannot tell a patched listing from an untouched
      // one: one create and one delete leave 8,192 either way, so both-applied
      // and neither-applied score identically. The identity of the two rows is
      // the assertion; the count is only the sanity check beside it.
      const createdRowPresent = held.some((row) => row.path === created.path);
      const deletedRowAbsent = held.every((row) => row.path !== "/repo/wide/file-00001");

      expect(expandedRows, "the bootstrap and one prefetched page").toBe(2 * PAGE);
      expect(afterExpand, "expanding cost more than the one prefetched page").toBe(1);
      expect(
        heldListingComplete,
        "the lane never reached the incomplete listing it is named for",
      ).toBe(false);
      expect(changeLists, "a single-file change in a paginated directory cost a list").toBe(0);
      expect(afterChangeRows, "the patches did not both land").toBe(2 * PAGE);
      expect(createdRowPresent, "the created row was never patched into the held pages").toBe(true);
      expect(deletedRowAbsent, "the deleted row was still on screen").toBe(true);
      console.log(`PHASE14_METRIC ${JSON.stringify({
        lane: "explorerPaginatedWatchTraffic",
        entries: all.length,
        hostPageSize: PAGE,
        heldListingComplete,
        expandDirectoryListRequests: afterExpand,
        expandedRows,
        changeDirectoryListRequests: changeLists,
        externalChangeRows: afterChangeRows,
        createdRowPath: createdRowPresent ? created.path : null,
        deletedRowPath: deletedRowAbsent ? "/repo/wide/file-00001" : null,
      })}`);
      await act(async () => { renderer.unmount(); });
    } finally {
      vi.useRealTimers();
    }
  });

  it("retains an ongoing connection-owned download across pane/session/root switch through published completion", async () => {
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const listeners: Array<(event: WorkspaceEvent) => void> = [];
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (active) => ({ ...root, paneId: active.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ fresh: true, snapshot: { rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true }, release: () => undefined })),
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
    await act(async () => {
      listeners.forEach((listener) => listener({ kind: "transfer", transfer: {
        id: "failed", scopeKey: transferScopeKey, path: "/repo/failed", kind: "file", state: "failed",
        outcome: "notPublished", completedBytes: "1", filesCompleted: "0",
      } }));
      listeners.forEach((listener) => listener({ kind: "transfer", transfer: {
        id: "cancelled", scopeKey: transferScopeKey, path: "/repo/cancelled", kind: "file", state: "cancelled",
        outcome: "notPublished", completedBytes: "1", filesCompleted: "0",
      } }));
      listeners.forEach((listener) => listener({ kind: "transfer", transfer: {
        id: "still-running", scopeKey: transferScopeKey, path: "/repo/running", kind: "file", state: "running",
        completedBytes: "1", filesCompleted: "0",
      } }));
    });
    expect(current?.transfers.map((transfer) => transfer.state)).toEqual(["running", "cancelled", "failed", "completed"]);
    await act(async () => { current?.clearFinishedTransfers(); });
    expect(current?.transfers).toMatchObject([{ id: "still-running", state: "running" }]);
    await act(async () => { renderer.unmount(); });
  });

  it("terminalizes a live row only when its owning connection is replaced", async () => {
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async (active) => ({ ...root, paneId: active.paneId })),
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ fresh: true, snapshot: { rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true }, release: () => undefined })),
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
      listDirectory: vi.fn(async (_scope, active, directory) => ({ rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true })),
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ fresh: true, snapshot: { rootToken: active.token, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true }, release: () => undefined })),
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

  it("holds the root probe behind the revealed pane's first paint", async () => {
    // The scope effect fires on the *optimistic* switch, and the root it
    // produces cascades into a directory listing and a Git lease — all on the
    // link that is carrying the pane's own reveal. The paint gate is what puts
    // the sidebar behind the screen the user actually asked for.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    armPanePaint("%1");
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(
      fixture.client.resolveActiveRoot,
      "the Explorer asked for a root ahead of the pane's own screen",
    ).not.toHaveBeenCalled();

    await act(async () => { notePanePainted("%1"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(1);
    expect(current?.root).toEqual(root);
    await act(async () => { renderer.unmount(); });
  });

  it("holds the root probe behind the paint even when the switch also moved the pane's cwd", async () => {
    // A workspace switch changes the active pane's `current_path` as well as
    // its scope, and the effect watching that path calls `rearm` *synchronously*
    // in the same commit. Ungated, that probe took the one-probe-at-a-time
    // latch and the gated call became a no-op — so the root, its directory
    // listing and the Git watch behind it (a whole `git status`, 60-80 KB on the
    // same ordered lane as the switch's own answer) all went out in front of the
    // screen the user asked for. Only every entry point being gated makes the
    // gate true.
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    function Harness({ scope, panePath }: { scope: FileWorkspaceScope; panePath: string }) {
      useWorkspaceFiles(fixture.client, scope, keyForScope(scope), panePath);
      return null;
    }
    armPanePaint("%1");
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<Harness scope={BASE_SCOPE} panePath="/repo" />);
      await Promise.resolve();
    });
    await act(async () => { notePanePainted("%1"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(1);

    const switched: FileWorkspaceScope = { ...BASE_SCOPE, sessionId: "$2", paneId: "%2" };
    armPanePaint("%2");
    await act(async () => {
      renderer.update(<Harness scope={switched} panePath="/elsewhere" />);
      await Promise.resolve();
    });
    await act(async () => { await Promise.resolve(); });
    expect(
      fixture.client.resolveActiveRoot,
      "the pane's cwd change probed ahead of the pane's own screen",
    ).toHaveBeenCalledTimes(1);

    await act(async () => { notePanePainted("%2"); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(2);
    await act(async () => { renderer.unmount(); });
  });

  it("resolves the root anyway when the pane never paints", async () => {
    // A timeout, never a barrier: a wedged renderer or a reveal the host never
    // answers must not also cost the user their file tree.
    vi.useFakeTimers();
    const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    const fixture = watchingClient(new Map([["/repo", [entry("/repo/a.txt")]]]), root);
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(fixture.client, BASE_SCOPE); return null; }
    armPanePaint("%1");
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(PANE_PAINT_TIMEOUT_MS - 1); });
    expect(fixture.client.resolveActiveRoot).not.toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    await act(async () => { await Promise.resolve(); });
    expect(fixture.client.resolveActiveRoot).toHaveBeenCalledTimes(1);
    expect(current?.root).toEqual(root);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
  });
});
