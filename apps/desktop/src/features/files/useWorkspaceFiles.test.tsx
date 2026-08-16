// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceClient, FileWorkspaceScope, WorkspaceEvent } from "./types";
import { keyForTransferConnection } from "./api";
import { useWorkspaceFiles } from "./useWorkspaceFiles";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** The hook's own constants, so the test moves with them rather than guessing. */
const DIRECTORY_REFRESH_COALESCE_MS = 150;
const ACTIVE_ROOT_POLL_MS = 2_000;

/**
 * Fake-timer clock that remembers where it is, so a test can place an event
 * relative to the root poll's phase rather than guessing at it.
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
    /** Stops `lead` ms short of the next active-root poll. */
    justBeforePoll: (lead: number) => advance(ACTIVE_ROOT_POLL_MS - (elapsed % ACTIVE_ROOT_POLL_MS) - lead),
  };
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

  it("refreshes the affected parent for precise native create/change/delete events", async () => {
    vi.useFakeTimers();
    const clock = fakeClock();
    const root = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
    let listener: ((event: WorkspaceEvent) => void) | undefined;
    let activeRoot = root;
    const listing = (active: ActiveRoot, directory: string) =>
      ({ rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true });
    const listDirectory = vi.fn(async (_scope, active: ActiveRoot, directory: string) => listing(active, directory));
    const client: FileWorkspaceClient = {
      resolveActiveRoot: vi.fn(async () => activeRoot), listDirectory,
      acquireDirectoryWatch: vi.fn(async (_scope, active, directory) => ({ snapshot: { rootToken: active.token, directory, revision: "1", entries: [], overflowRecovery: false, complete: true }, release: () => undefined })), openFile: vi.fn(), writeText: vi.fn(), mutate: vi.fn(), startDownload: vi.fn(), cancelTransfer: vi.fn(),
      subscribe: vi.fn(async (_scope, next) => { listener = next; return () => undefined; }),
    };
    const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 41, sessionId: "$1", paneId: "%1" };
    let current: ReturnType<typeof useWorkspaceFiles> | undefined;
    function Harness() { current = useWorkspaceFiles(client, scope); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); await Promise.resolve(); });
    const reads = () => listDirectory.mock.calls.filter((call) => call[2] === "/repo").length;
    const before = reads();
    // A burst, as an agent writing into the workspace root produces. They are
    // one directory's worth of news and must cost one re-read, not five: over
    // SSH each one is a round trip, and each one used to replace the whole list.
    await act(async () => {
      for (let index = 0; index < 5; index += 1) {
        listener?.({ kind: "fileChanged", rootToken: "root", path: `/repo/new-${index}.txt`, generation: "2" });
      }
      await Promise.resolve();
    });
    expect(reads(), "a burst re-read the directory before its window closed").toBe(before);
    await act(async () => { await clock.settle(); });
    expect(reads()).toBe(before + 1);
    // And the window reopens, so a directory under continuous change still
    // refreshes rather than being starved by the events behind it.
    await act(async () => { listener?.({ kind: "fileChanged", rootToken: "root", path: "/repo/later.txt", generation: "3" }); await clock.settle(); });
    expect(reads()).toBe(before + 2);
    // The wait put a root change between the check that admitted the event and
    // the request it authorised. An event for the root that has since been left
    // must not be read against the root that replaced it — the completion guards
    // cannot catch that one, because they compare against the root the request
    // carried, which is the new one.
    // Timed so the root actually moves *inside* the window rather than before
    // it opens or after it shuts: sit just short of the root poll, raise the
    // event, then let the poll land and only then let the window close.
    await act(async () => { await clock.justBeforePoll(20); });
    activeRoot = { token: "next", paneId: "%1", cwd: "/other", path: "/other", gitWorktree: true, revision: "2" };
    await act(async () => {
      listener?.({ kind: "fileChanged", rootToken: "root", path: "/repo/racing.txt", generation: "4" });
      await clock.advance(25);
    });
    expect(current?.root?.token, "the fixture never moved the root inside the window").toBe("next");
    await act(async () => { await clock.settle(); });
    expect(current?.root?.token).toBe("next");
    expect(
      listDirectory.mock.calls.some((call) => call[2] === "/repo" && (call[1] as ActiveRoot).token === "next"),
      "a path from the previous root was listed against the root that replaced it",
    ).toBe(false);
    await act(async () => { renderer.unmount(); });
    vi.useRealTimers();
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
