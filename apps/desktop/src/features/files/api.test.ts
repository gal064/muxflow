import { beforeEach, describe, expect, it, vi } from "vitest";
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

describe("TauriFileWorkspaceClient", () => {
  it("maps active roots and preserves decimal u64 metadata without numeric coercion", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "op", activeRoot: { paneId: "%1", root: "/repo", rootToken: "token", gitWorktree: true, serverIdentity: "server", topologyGeneration: "7", rootGeneration: "18446744073709551615" } });
    const client = new TauriFileWorkspaceClient();
    await expect(client.resolveActiveRoot(scope)).resolves.toEqual({ token: "token", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "18446744073709551615" });
    expect(invokeMock).toHaveBeenCalledWith("file_request", { clientId: "client", command: expect.objectContaining({ operation: "resolveActiveRoot", paneId: "%1", expectedServerIdentity: "server", expectedTopologyGeneration: "7" }) });
  });

  it("maps lazy directory pages including collapsed protected entries", async () => {
    invokeMock.mockResolvedValueOnce({ operationId: "op", directory: { watchId: "", root: "/repo", path: "/repo", generation: "12", overflowed: false, authoritative: true, nextPageToken: "opaque", complete: false, entries: [
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
      const channel = (args as { purpose: string; onEvent: { onmessage?: (value: ArrayBuffer) => void } }).onEvent;
      expect((args as { purpose: string }).purpose).toBe("text");
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
    client.publishWireEvent({ operationId: "", rootToken: "token", watchId: "w", directory: { watchId: "w", root: "/repo", path: "/repo", generation: "2", entries: [], overflowed: true, authoritative: true, nextPageToken: "", complete: true }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    client.publishWireEvent({ operationId: "agent", rootToken: "token", metadata: { path: "/repo/a", name: "a", kind: "file", size: "1", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "99", mime: "", imagePreviewEligible: false }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    client.publishWireEvent({ operationId: "delete", rootToken: "token", deleted: true, metadata: { path: "/repo/gone", name: "gone", kind: "file", size: "1", modifiedUnixMillis: "1", mode: 0o644, symlink: false, symlinkTarget: "", expandable: false, generation: "100", mime: "", imagePreviewEligible: false }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" });
    expect(events).toEqual([
      { kind: "directoryChanged", rootToken: "token", directory: "/repo", overflow: true },
      { kind: "fileChanged", rootToken: "token", path: "/repo/a", generation: "99", operationId: "agent" },
      { kind: "fileDeleted", rootToken: "token", path: "/repo/gone" },
    ]);
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
