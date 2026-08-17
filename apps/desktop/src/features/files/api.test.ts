import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, perfCounterSnapshot, perfHighWaterSnapshot, resetPerfProbe } from "../../perf/probe";
import { TauriFileWorkspaceClient } from "./api";
import type { ActiveRoot, FileWorkspaceScope, WorkspaceEvent } from "./types";

const { invokeMock, channels } = vi.hoisted(() => ({ invokeMock: vi.fn(), channels: [] as Array<{ onmessage?: (message: unknown) => void }> }));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class MockChannel<T> { onmessage?: (message: T) => void; constructor() { channels.push(this as { onmessage?: (message: unknown) => void }); } },
  invoke: invokeMock,
}));

const scope: FileWorkspaceScope = { clientId: "client", hostProfileId: "local", serverIdentity: "server", generation: 7, terminalEpoch: 41, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "token", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "9" };

beforeEach(() => { invokeMock.mockReset(); channels.length = 0; });
afterEach(() => resetPerfProbe());

describe("TauriFileWorkspaceClient", () => {
  it("accounts for Phase 14 directory list and shared-watch operations exactly", async () => {
    enablePerfProbe(async () => undefined);
    const directory = {
      watchId: "watch", root: "/repo", path: "/repo", generation: "12", overflowed: false,
      authoritative: true, nextPageToken: "", complete: true,
      entries: [{ path: "/repo/a", name: "a", kind: "file", size: "4", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "1", mime: "text/plain", imagePreviewEligible: false }],
    };
    invokeMock
      .mockResolvedValueOnce({ operationId: "list", directory })
      .mockResolvedValueOnce({ operationId: "watch", directory })
      .mockResolvedValueOnce({ operationId: "unwatch" });
    const client = new TauriFileWorkspaceClient();
    await client.listDirectory(scope, root, "/repo");
    const [first, second] = await Promise.all([
      client.acquireDirectoryWatch(scope, root, "/repo"),
      client.acquireDirectoryWatch(scope, root, "/repo"),
    ]);
    first.release();
    second.release();
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(3));
    await vi.waitFor(() => expect(perfCounterSnapshot()["explorer.listMappedPayloadBytes"]).toBeGreaterThan(0));
    const counters = perfCounterSnapshot();
    const highWater = perfHighWaterSnapshot();
    expect(counters["explorer.directoryListRequests"]).toBe(1);
    expect(counters["explorer.watchSubscribers"]).toBe(2);
    expect(counters["explorer.watchRequests"]).toBe(1);
    expect(counters["explorer.watchReleases"]).toBe(2);
    expect(counters["explorer.listPayloadEntries"]).toBe(1);
    expect(counters["desktop.hostRequestAttempts"]).toBe(3);
    expect(counters["desktop.hostRequestSuccesses"]).toBe(3);
    expect(counters["desktop.hostRequestFailures"]).toBeUndefined();
    expect(counters["desktop.hostRequestCancellations"]).toBeUndefined();
    const exactBoundaryBytes = invokeMock.mock.calls.reduce((total, [, boundary]) =>
      total + new TextEncoder().encode(JSON.stringify(boundary)).byteLength, 0);
    expect(counters["desktop.hostRequestBytes"]).toBe(exactBoundaryBytes);
    expect(highWater["explorer.activeWatches"]).toBe(1);
    console.log(`PHASE14_METRIC ${JSON.stringify({
      lane: "explorerListWatch", directoryListRequests: counters["explorer.directoryListRequests"],
      watchSubscribers: counters["explorer.watchSubscribers"], watchRequests: counters["explorer.watchRequests"],
      watchReleases: counters["explorer.watchReleases"], activeWatchHighWater: highWater["explorer.activeWatches"],
      hostRequestAttempts: counters["desktop.hostRequestAttempts"],
      hostRequestSuccesses: counters["desktop.hostRequestSuccesses"],
      hostRequestFailures: counters["desktop.hostRequestFailures"] ?? 0,
      hostRequestCancellations: counters["desktop.hostRequestCancellations"] ?? 0,
      mappedPayloadBytes: counters["explorer.listMappedPayloadBytes"],
    })}`);
  });

  it("marks a snapshot from an already-established watch as not fresh", async () => {
    // The bootstrap is produced once, when the watch is armed. A subscriber
    // joining ten minutes later is handed that same listing, so treating it as
    // this acquisition's answer silently installed a ten-minute-old directory
    // and reverted every row patched since.
    const snapshot = (generation: string) => ({
      watchId: "watch", root: "/repo", path: "/repo", generation, overflowed: false,
      authoritative: true, nextPageToken: "", complete: true, entries: [],
    });
    invokeMock.mockResolvedValue({ operationId: "watch", directory: snapshot("1") });
    const client = new TauriFileWorkspaceClient();
    const first = await client.acquireDirectoryWatch(scope, root, "/repo");
    expect(first.fresh).toBe(true);
    const second = await client.acquireDirectoryWatch(scope, root, "/repo");
    expect(second.fresh, "a joined watch answered as if its snapshot were current").toBe(false);
    expect(second.snapshot.revision).toBe("1");
    // Still exactly one host watch, and one unwatch when the last lease goes.
    expect(invokeMock).toHaveBeenCalledTimes(1);
    first.release();
    second.release();
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith(
      "file_request", { clientId: "client", command: expect.objectContaining({ operation: "unwatchDirectory" }) }));
  });

  it("keeps a shared watch alive when one of its subscribers abandons it", async () => {
    // Cancellation used to be wired to whichever caller happened to arm the
    // watch, so that caller giving up cancelled the request out from under
    // every other subscriber — and the survivor was never told, so it sat
    // there receiving no events at all for the life of the tab.
    let settle!: (value: unknown) => void;
    invokeMock.mockImplementation(async (command) => {
      if (command === "cancel_file_request") return undefined;
      return new Promise((resolve) => { settle = resolve; });
    });
    const abort = new AbortController();
    const client = new TauriFileWorkspaceClient();
    const abandoned = client.acquireDirectoryWatch(scope, root, "/repo", { signal: abort.signal });
    const kept = client.acquireDirectoryWatch(scope, root, "/repo");
    abort.abort();
    await expect(abandoned).rejects.toMatchObject({ name: "AbortError" });
    expect(invokeMock).not.toHaveBeenCalledWith("cancel_file_request", expect.anything());
    settle({ operationId: "watch", directory: { watchId: "watch", root: "/repo", path: "/repo", generation: "4", authoritative: true, nextPageToken: "", complete: true, entries: [] } });
    const lease = await kept;
    expect(lease.snapshot.revision).toBe("4");
    expect(lease.fresh).toBe(true);
  });

  it("gives a watch back to the host even when its own bootstrap failed", async () => {
    // The watch ID is minted here and the host registers the watch before it
    // lists, so a control-lane timeout on a large directory leaves a
    // registration the desktop has stopped counting. Against the host's watch
    // budget those orphans end as "every expansion is refused".
    const failure = new Error("watch bootstrap timed out");
    invokeMock.mockImplementation(async (command, args) => {
      if ((args as { command?: { operation?: string } }).command?.operation === "unwatchDirectory") return { operationId: "unwatch" };
      throw failure;
    });
    const client = new TauriFileWorkspaceClient();
    await expect(client.acquireDirectoryWatch(scope, root, "/repo")).rejects.toThrow("timed out");
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith(
      "file_request", { clientId: "client", command: expect.objectContaining({ operation: "unwatchDirectory" }) }));
    // And the failed record is gone, so the next expansion arms a real watch.
    invokeMock.mockResolvedValue({ operationId: "watch", directory: { watchId: "watch", root: "/repo", path: "/repo", generation: "2", authoritative: true, nextPageToken: "", complete: true, entries: [] } });
    await expect(client.acquireDirectoryWatch(scope, root, "/repo")).resolves.toMatchObject({ fresh: true });
  });

  it("maps active roots and preserves decimal u64 metadata without numeric coercion", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "op", activeRoot: { paneId: "%1", root: "/repo", rootToken: "token", gitWorktree: true, serverIdentity: "server", topologyGeneration: "7", rootGeneration: "18446744073709551615" } });
    const client = new TauriFileWorkspaceClient();
    await expect(client.resolveActiveRoot(scope)).resolves.toEqual({ token: "token", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "18446744073709551615" });
    expect(invokeMock).toHaveBeenCalledWith("file_request", { clientId: "client", command: expect.objectContaining({ operation: "resolveActiveRoot", paneId: "%1", expectedServerIdentity: "server", expectedTopologyGeneration: "7" }) });
  });

  it("counts a malformed host response as a request failure, never a success", async () => {
    enablePerfProbe(async () => undefined);
    invokeMock.mockResolvedValueOnce({ operationId: "missing-root" });

    await expect(new TauriFileWorkspaceClient().resolveActiveRoot(scope)).rejects.toThrow("omitted the active root");

    expect(perfCounterSnapshot()).toMatchObject({
      "desktop.hostRequestAttempts": 1,
      "desktop.hostRequestFailures": 1,
      "file.hostRequestAttempts": 1,
      "file.hostRequestFailures": 1,
    });
    expect(perfCounterSnapshot()["desktop.hostRequestSuccesses"]).toBeUndefined();
  });

  it("maps lazy directory pages including collapsed protected entries", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "op", directory: { watchId: "", root: "/repo", path: "/repo", generation: "12", authoritative: true, nextPageToken: "opaque", complete: false, entries: [
      { path: "/repo/.git", name: ".git", kind: "directory", size: "4096", modifiedUnixMillis: "1", mode: 0o755, symlink: false, symlinkTarget: "", expandable: false, generation: "18446744073709551615", mime: "", imagePreviewEligible: false },
    ] } });
    const listing = await new TauriFileWorkspaceClient().listDirectory(scope, root, "/repo");
    expect(listing).toMatchObject({ rootToken: "token", nextPageToken: "opaque", complete: false, entries: [{ name: ".git", expandable: false, sizeBytes: "4096" }] });
  });

  it("passes destructive confirmations and encodes CRLF text atomically", async () => {
    const metadata = { path: "/repo/a", name: "a", kind: "file", size: "4", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "3", mime: "", imagePreviewEligible: false };
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "file_request") return { operationId: "delete", metadata };
      if (command === "start_file_write") {
        const channel = (args as { onEvent: { onmessage?: (value: ArrayBuffer) => void } }).onEvent;
        queueMicrotask(() => channel.onmessage?.(jsonFrame(3, { transferId: "transfer", operationId: "save", state: "completed", generation: "3", metadata })));
        return "transfer";
      }
      throw new Error(`unexpected command ${String(command)} args=${JSON.stringify(args)}`);
    });
    const client = new TauriFileWorkspaceClient();
    await client.mutate(scope, root, { kind: "delete", path: "/repo/dir", confirmedNonEmpty: true });
    await client.writeText(scope, root, { path: "/repo/a", content: "a\nb", baseGeneration: "2", operationId: "save", lineEnding: "crlf" });
    expect(invokeMock.mock.calls[0][1]).toMatchObject({ command: { operation: "mutate", mutation: "delete", nonEmptyConfirmed: true, root: "/repo", rootToken: "token" } });
    expect((invokeMock.mock.calls[1][1] as { content: number[] }).content).toEqual([...new TextEncoder().encode("a\r\nb")]);
    expect(invokeMock.mock.calls[1][1]).toMatchObject({ clientId: "client", expectedServerIdentity: "server", connectionEpoch: "41" });
  });

  it("reassembles sequenced text chunks from the independent bulk lane", async () => {
    const metadata = { path: "/repo/a", name: "a", kind: "file", size: "4", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "18446744073709551615", mime: "text/plain", imagePreviewEligible: false };
    invokeMock.mockImplementation(async (command, args) => {
      expect(command).toBe("start_file_read");
      const channel = (args as { onEvent: { onmessage?: (value: ArrayBuffer) => void } }).onEvent;
      queueMicrotask(() => {
        channel.onmessage?.(jsonFrame(1, { transferId: "read", state: "metadata", metadata, contentKind: "text" }));
        channel.onmessage?.(chunkFrame(0n, new TextEncoder().encode("a\r\nb")));
        channel.onmessage?.(jsonFrame(3, { transferId: "read", state: "completed", totalBytes: "4", generation: metadata.generation, blake3: "verified" }));
      });
      return "read";
    });
    await expect(new TauriFileWorkspaceClient().openFile(scope, root, "/repo/a")).resolves.toEqual({
      kind: "text", file: { path: "/repo/a", content: "a\r\nb", generation: metadata.generation, sizeBytes: "4", lineEnding: "crlf", encoding: "utf-8" },
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("cancels an obsolete bulk read when its editor load is superseded", async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === "start_file_read") return "obsolete-read";
      if (command === "cancel_file_io") return undefined;
      throw new Error(`unexpected command ${String(command)}`);
    });
    const abort = new AbortController();
    const opened = new TauriFileWorkspaceClient().openFile(scope, root, "/repo/a", abort.signal);
    await Promise.resolve();
    abort.abort();
    await expect(opened).rejects.toMatchObject({ name: "AbortError" });
    await Promise.resolve();
    expect(invokeMock).toHaveBeenCalledWith("cancel_file_io", { transferId: "obsolete-read" });
  });

  it("maps sequenced directory/file events only through known root capabilities", async () => {
    const client = new TauriFileWorkspaceClient();
    invokeMock.mockResolvedValueOnce({ operationId: "op", activeRoot: { paneId: "%1", root: "/repo", rootToken: "token", gitWorktree: true, serverIdentity: "server", topologyGeneration: "7", rootGeneration: "1" } });
    await client.resolveActiveRoot(scope);
    const events: unknown[] = [];
    await client.subscribe(scope, (event) => events.push(event));
    client.publishWireEvent({ operationId: "", rootToken: "token", watchId: "w", directory: { watchId: "w", root: "/repo", path: "/repo", generation: "2", entries: [], authoritative: true, nextPageToken: "", complete: true }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    client.publishWireEvent({ operationId: "agent", rootToken: "token", metadata: { path: "/repo/a", name: "a", kind: "file", size: "1", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "99", mime: "", imagePreviewEligible: false }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    client.publishWireEvent({ operationId: "delete", rootToken: "token", deleted: true, metadata: { path: "/repo/gone", name: "gone", kind: "file", size: "1", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "100", mime: "", imagePreviewEligible: false }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    expect(events).toEqual([
      {
        kind: "directorySnapshot",
        rootToken: "token",
        listing: {
          rootToken: "token", directory: "/repo", revision: "2", entries: [],
          recoveredFromOverflow: false, nextPageToken: undefined, complete: true,
        },
      },
      {
        kind: "fileChanged", rootToken: "token", path: "/repo/a", generation: "99", operationId: "agent",
        entry: {
          path: "/repo/a", name: "a", kind: "file", sizeBytes: "1", modifiedMillis: "1",
          generation: "99", executable: false, expandable: false,
        },
      },
      { kind: "fileDeleted", rootToken: "token", path: "/repo/gone" },
    ]);
  });

  it("carries an authoritative rescan through as its listing rather than an invalidation", async () => {
    const client = new TauriFileWorkspaceClient();
    const events: WorkspaceEvent[] = [];
    await client.subscribe(scope, (event) => events.push(event));
    client.publishWireEvent({
      operationId: "", rootToken: "token", watchId: "w",
      directory: {
        watchId: "w", root: "/repo", path: "/repo", generation: "9", authoritative: true,
        nextPageToken: "", complete: true,
        entries: [{ path: "/repo/kept", name: "kept", kind: "file", size: "2", modifiedUnixMillis: "5", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "77", mime: "", imagePreviewEligible: false }],
      },
      transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "",
    });
    const published = events[0];
    expect(published?.kind).toBe("directorySnapshot");
    if (published?.kind !== "directorySnapshot") throw new Error("expected a mapped snapshot");
    expect(published.listing.entries).toEqual([expect.objectContaining({ name: "kept", generation: "77" })]);
  });

  it("drops a precise event whose metadata cannot be drawn instead of inventing a row", async () => {
    const client = new TauriFileWorkspaceClient();
    const events: WorkspaceEvent[] = [];
    await client.subscribe(scope, (event) => events.push(event));
    client.publishWireEvent({
      operationId: "", rootToken: "token",
      metadata: { path: "/repo/unmapped", name: "", kind: "unspecified", size: "0", modifiedUnixMillis: "0", mode: 0, symlink: false, symlinkTarget: "", expandable: false, generation: "0", mime: "", imagePreviewEligible: false },
      transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "",
    });
    expect(events).toEqual([{ kind: "fileChanged", rootToken: "token", path: "/repo/unmapped", generation: "0" }]);
  });

  it("opens an eligible image in one bulk request, with no separate text probe", async () => {
    const metadata = { path: "/repo/logo.png", name: "logo.png", kind: "file", size: "3", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "12", mime: "image/png", imagePreviewEligible: true };
    invokeMock.mockImplementation(async (command, args) => {
      expect(command).toBe("start_file_read");
      expect(args).not.toHaveProperty("purpose");
      const channel = (args as { onEvent: { onmessage?: (value: ArrayBuffer) => void } }).onEvent;
      queueMicrotask(() => {
        channel.onmessage?.(jsonFrame(1, { transferId: "read", state: "metadata", metadata, contentKind: "image" }));
        channel.onmessage?.(chunkFrame(0n, new Uint8Array([1, 2, 3])));
        channel.onmessage?.(jsonFrame(3, { transferId: "read", state: "completed", totalBytes: "3", generation: "12", metadata, contentKind: "image", metadataOnly: false }));
      });
      return "read";
    });
    const opened = await new TauriFileWorkspaceClient().openFile(scope, root, "/repo/logo.png");
    expect(opened).toMatchObject({ kind: "binary", file: { previewKind: "image", mime: "image/png", generation: "12" } });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("passes the caller's own root capability so an unchanged root costs no rediscovery", async () => {
    enablePerfProbe(async () => undefined);
    invokeMock.mockResolvedValueOnce({
      operationId: "op", rootUnchanged: true,
      activeRoot: { paneId: "%1", root: "/repo", rootToken: "token", gitWorktree: true, serverIdentity: "server", topologyGeneration: "7", rootGeneration: "3" },
    });
    await new TauriFileWorkspaceClient().resolveActiveRoot(scope, { knownRootToken: "token" });
    expect(invokeMock).toHaveBeenCalledWith("file_request", {
      clientId: "client",
      command: expect.objectContaining({ operation: "resolveActiveRoot", knownRootToken: "token" }),
    });
    expect(perfCounterSnapshot()["explorer.rootProbeUnchanged"]).toBe(1);
  });

  it("stops bounded remote enumeration when a directory read is abandoned", async () => {
    let settle!: () => void;
    invokeMock.mockImplementation(async (command) => {
      if (command === "cancel_file_request") return undefined;
      return new Promise((resolve) => { settle = () => resolve({ operationId: "list", directory: { watchId: "", root: "/repo", path: "/repo", generation: "1", entries: [], authoritative: true, nextPageToken: "", complete: true } }); });
    });
    const abort = new AbortController();
    const client = new TauriFileWorkspaceClient();
    const listing = client.listDirectory(scope, root, "/repo", { signal: abort.signal });
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("file_request", expect.anything()));
    const operationId = (invokeMock.mock.calls[0][1] as { command: { operationId: string } }).command.operationId;
    abort.abort();
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("cancel_file_request", { clientId: "client", operationId }));
    // And the caller is answered now rather than when the host gets round to
    // it: a read that resolved anyway could still install rows into a
    // directory the tree had already collapsed.
    await expect(listing).rejects.toMatchObject({ name: "AbortError" });
    settle();
  });

  it("uses the canonical verifying/unknown outcome schema and preserves cleanup failure", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      expect(command).toBe("start_download");
      const channel = (args as { onEvent: { onmessage?: (value: unknown) => void } }).onEvent;
      queueMicrotask(() => {
        channel.onmessage?.({
          transferId: "download-1", state: "verifying", terminal: false,
          serverIdentity: "server", expectedServerIdentity: "server", connectionEpoch: "41", transferredBytes: "12", totalBytes: "12",
        });
        channel.onmessage?.({
          transferId: "download-1", state: "failed", outcome: "unknown", failureKind: "outcomeUnknown", terminal: true,
          serverIdentity: "server", expectedServerIdentity: "server", connectionEpoch: "41", transferredBytes: "12", totalBytes: "12",
          cleanupStatus: "failed", cleanupError: "could not remove partial", error: "publish response was lost",
        });
      });
      return "download-1";
    });
    const client = new TauriFileWorkspaceClient();
    const events: unknown[] = [];
    await client.subscribe(scope, (next) => events.push(next));
    await expect(client.startDownload(scope, root, { path: "/repo/a", destination: "/tmp/a", kind: "file", collision: "fail" })).resolves.toMatchObject({
      id: "download-1", state: "failed", outcome: "unknown", failureKind: "outcomeUnknown",
      cleanupStatus: "failed", cleanupError: "could not remove partial",
    });
    expect(events).toContainEqual({ kind: "transfer", transfer: expect.objectContaining({ state: "verifying" }) });
    expect(events).toContainEqual({ kind: "transfer", transfer: expect.objectContaining({ state: "failed", outcome: "unknown" }) });
  });

  it.each(["queued", "running"] as const)("preserves a canonical %s download cancellation without synthetic failure", async (initialState) => {
    invokeMock.mockImplementation(async (_command, args) => {
      const channel = (args as { onEvent: { onmessage?: (value: unknown) => void } }).onEvent;
      queueMicrotask(() => {
        channel.onmessage?.({
          transferId: "download-cancel", state: initialState, terminal: false, transferredBytes: "2",
          serverIdentity: "server", expectedServerIdentity: "server", connectionEpoch: "41",
        });
        channel.onmessage?.({
          transferId: "download-cancel", state: "cancelled", outcome: "notPublished", terminal: true,
          transferredBytes: "2", cleanupStatus: "removed", error: "cancelled by user",
          serverIdentity: "server", expectedServerIdentity: "server", connectionEpoch: "41",
        });
      });
      return "download-cancel";
    });
    const client = new TauriFileWorkspaceClient();
    const events: WorkspaceEvent[] = [];
    await client.subscribe(scope, (next) => events.push(next));
    await expect(client.startDownload(scope, root, {
      path: "/repo/a", destination: "/tmp/a", kind: "file", collision: "fail",
    })).resolves.toMatchObject({ state: "cancelled", outcome: "notPublished", cleanupStatus: "removed" });
    const last = events.at(-1);
    const transfer = last?.kind === "transfer" ? last.transfer : undefined;
    expect(transfer).not.toHaveProperty("failureKind");
  });

  it("fails closed on stale-bound and unknown download events instead of casting them active", async () => {
    for (const wire of [
      { state: "running", serverIdentity: "old", expectedServerIdentity: "server", connectionEpoch: "41", terminal: false },
      { state: "futureState", serverIdentity: "server", expectedServerIdentity: "server", connectionEpoch: "41", terminal: false },
    ]) {
      invokeMock.mockImplementationOnce(async (_command, args) => {
        const channel = (args as { onEvent: { onmessage?: (value: unknown) => void } }).onEvent;
        queueMicrotask(() => channel.onmessage?.({ transferId: "bad", transferredBytes: "0", ...wire }));
        return "bad";
      });
      const transfer = await new TauriFileWorkspaceClient().startDownload(scope, root, {
        path: "/repo/a", destination: "/tmp/a", kind: "file", collision: "fail",
      });
      expect(transfer).toMatchObject({ state: "failed", outcome: "notPublished", failureKind: "transfer" });
      expect(transfer.error).toMatch(/stale connection scope|Unknown transfer state/u);
    }
  });
});

function jsonFrame(kind: number, value: unknown): ArrayBuffer {
  const json = new TextEncoder().encode(JSON.stringify(value));
  const frame = new Uint8Array(1 + json.byteLength);
  frame[0] = kind;
  frame.set(json, 1);
  return frame.buffer;
}

function chunkFrame(offset: bigint, value: Uint8Array): ArrayBuffer {
  const frame = new Uint8Array(9 + value.byteLength);
  frame[0] = 2;
  new DataView(frame.buffer).setBigUint64(1, offset, false);
  frame.set(value, 9);
  return frame.buffer;
}
