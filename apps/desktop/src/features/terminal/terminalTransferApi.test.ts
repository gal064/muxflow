import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TauriTerminalTransferClient } from "./terminalTransferApi";
import type { TerminalTransferScope } from "./terminalTransfers";

const channels = vi.hoisted(() => [] as Array<{ onmessage?: (value: unknown) => void }>);
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class MockChannel { onmessage?: (value: unknown) => void; constructor() { channels.push(this); } },
  invoke: vi.fn(),
}));

const scope: TerminalTransferScope = {
  clientId: "client", hostProfileId: "remote", serverIdentity: "tmux:one", connectionEpoch: "9007199254740993", mode: "ssh",
  paneId: "%7", renderLifetime: "render-7",
};

const event = (value: Record<string, unknown>) => ({
  transferId: "transfer-1", sourcePath: "/local/a", name: "a", state: "running",
  transferredBytes: "0", totalBytes: "20", serverIdentity: scope.serverIdentity,
  expectedServerIdentity: scope.serverIdentity, connectionEpoch: scope.connectionEpoch, terminal: false, ...value,
});

describe("TauriTerminalTransferClient", () => {
  beforeEach(() => { channels.length = 0; vi.mocked(invoke).mockReset(); });

  it("uses the tokenized preflight command and resolves only its terminal scoped event", async () => {
    vi.mocked(invoke).mockResolvedValue("preflight-1");
    const progress = vi.fn();
    const pending = new TauriTerminalTransferClient().preflight(scope, "/local/a", "a", {
      collision: "rename", largeUploadConfirmed: true, imagePng: false,
    }, progress);
    await Promise.resolve();
    expect(invoke).toHaveBeenCalledWith("start_terminal_upload_preflight", {
      clientId: "client", profileId: "remote", expectedServerIdentity: "tmux:one", connectionEpoch: "9007199254740993",
      sourcePath: "/local/a", destinationName: "a", collision: "rename", largeUploadConfirmed: true, imagePng: false,
      onEvent: channels[0],
    });
    channels[0].onmessage?.(event({ transferId: "preflight-1", state: "preflighting" }));
    expect(progress).toHaveBeenLastCalledWith(expect.objectContaining({ id: "preflight-1", state: "preflighting" }));
    channels[0].onmessage?.(event({
      transferId: "preflight-1", state: "completed", outcome: "notPublished", terminal: true,
      sizeBytes: "9007199254740993", name: "a-2", destination: "a-2", collisionRenamed: true,
      cleanupStatus: "removed",
    }));
    await expect(pending).resolves.toMatchObject({ name: "a", destination: "a-2", collision: true, sourceKind: "regularFile", readable: true });
  });

  it("cancels a silent preflight by its token when its captured scope is aborted", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => command === "start_terminal_upload_preflight"
      ? "preflight-silent"
      : { disposition: "cancelRequested", phase: "preflighting" });
    const abort = new AbortController();
    const pending = new TauriTerminalTransferClient().preflight(scope, "/local/a", "a", {
      collision: "rename", largeUploadConfirmed: true, imagePng: false,
    }, vi.fn(), abort.signal);
    await Promise.resolve();
    abort.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(invoke).toHaveBeenCalledWith("cancel_terminal_upload_preflight", { preflightId: "preflight-silent" });
  });

  it("does not resolve or expose a destination before verifying and completed events", async () => {
    vi.mocked(invoke).mockResolvedValue("transfer-1");
    const progress = vi.fn();
    let settled = false;
    const upload = new TauriTerminalTransferClient().start(scope, "/local/a", "a", {
      collision: "fail", largeUploadConfirmed: false, imagePng: false,
    }, progress).then((value) => { settled = true; return value; });
    await Promise.resolve();
    channels[0].onmessage?.(event({ state: "running", transferredBytes: "12", throughputBytesPerSecond: 12.9 }));
    expect(progress).toHaveBeenLastCalledWith(expect.objectContaining({ state: "running", completedBytes: "12", bytesPerSecond: "12" }));
    channels[0].onmessage?.(event({ state: "verifying", transferredBytes: "20" }));
    expect(progress).toHaveBeenLastCalledWith(expect.objectContaining({ state: "verifying" }));
    expect(settled).toBe(false);
    channels[0].onmessage?.(event({ state: "completed", outcome: "published", terminal: true, transferredBytes: "20", destination: "/remote/a", blake3: "digest" }));
    await expect(upload).resolves.toEqual({ id: "transfer-1", destination: "/remote/a", digest: "digest" });
  });

  it("rejects stale bindings, future states, malformed u64 values, and unverified completion", async () => {
    for (const patch of [
      { serverIdentity: "other" },
      { state: "future" },
      { state: "running", transferredBytes: "9e18" },
      { state: "completed", outcome: "published", terminal: true },
    ]) {
      vi.mocked(invoke).mockResolvedValueOnce("transfer-1");
      const upload = new TauriTerminalTransferClient().start(scope, "/local/a", "a", {
        collision: "fail", largeUploadConfirmed: false, imagePng: false,
      }, vi.fn());
      channels.at(-1)!.onmessage?.(event(patch));
      await expect(upload).rejects.toThrow();
    }
  });

  it("keeps a lost commit response as an explicit unknown outcome and surfaces cleanup failure", async () => {
    vi.mocked(invoke).mockResolvedValue("transfer-1");
    const progress = vi.fn();
    const upload = new TauriTerminalTransferClient().start(scope, "/local/a", "a", {
      collision: "fail", largeUploadConfirmed: false, imagePng: false,
    }, progress);
    channels[0].onmessage?.(event({
      state: "failed", outcome: "unknown", failureKind: "outcomeUnknown", terminal: true,
      transferredBytes: "20", cleanupStatus: "failed", cleanupError: "ownership reconciliation unavailable",
      error: "upload commit response was lost",
    }));
    await expect(upload).rejects.toThrow("commit response was lost");
    expect(progress).toHaveBeenCalledWith(expect.objectContaining({
      state: "failed", outcome: "unknown", failureKind: "outcomeUnknown",
      cleanupStatus: "failed", cleanupError: "ownership reconciliation unavailable",
    }));
  });

  it("validates typed cancel dispositions and two unique UUID clipboard destinations", async () => {
    const names = [
      "clipboard-00000000-0000-4000-8000-000000000001.png",
      "clipboard-00000000-0000-4000-8000-000000000002.png",
    ];
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "stage_clipboard_png") {
        const name = names.shift()!;
        return { path: `/cache/${name}`, sizeBytes: "8", name };
      }
      if (command === "cancel_terminal_upload") return { disposition: "awaitingAuthoritativeOutcome", phase: "verifying" };
      return undefined;
    });
    const client = new TauriTerminalTransferClient();
    const first = await client.stageClipboardPng(Uint8Array.of(137, 80));
    const second = await client.stageClipboardPng(Uint8Array.of(137, 80));
    expect(first.name).not.toBe(second.name);
    expect(first.name).not.toContain(" ");
    expect(second.name).not.toBe("clipboard (1).png");
    expect(invoke).toHaveBeenCalledWith("stage_clipboard_png", Uint8Array.of(137, 80));
    await expect(client.cancel("transfer-9")).resolves.toEqual({ disposition: "awaitingAuthoritativeOutcome", phase: "verifying" });
  });

  it("validates native Linux file and staged-image clipboard payloads", async () => {
    const name = "clipboard-00000000-0000-4000-8000-000000000001.png";
    vi.mocked(invoke)
      .mockResolvedValueOnce({ kind: "files", uris: ["file:///tmp/a", "file:///tmp/b"] })
      .mockResolvedValueOnce({ kind: "image", staged: { path: `/cache/${name}`, sizeBytes: "9007199254740993", name } });
    const client = new TauriTerminalTransferClient();
    await expect(client.readNativeClipboard()).resolves.toEqual({ kind: "files", uris: ["file:///tmp/a", "file:///tmp/b"] });
    await expect(client.readNativeClipboard()).resolves.toEqual({
      kind: "image", staged: { path: `/cache/${name}`, sizeBytes: "9007199254740993", name },
    });
    expect(invoke).toHaveBeenNthCalledWith(1, "read_native_terminal_clipboard");
  });

  it("validates native clipboard text payloads", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({ kind: "text", text: "echo copied elsewhere" })
      .mockResolvedValueOnce({ kind: "text", text: "" })
      .mockResolvedValueOnce({ kind: "text" });
    const client = new TauriTerminalTransferClient();
    await expect(client.readNativeClipboard()).resolves.toEqual({ kind: "text", text: "echo copied elsewhere" });
    await expect(client.readNativeClipboard()).rejects.toThrow("invalid text");
    await expect(client.readNativeClipboard()).rejects.toThrow("invalid text");
  });

  it.each(["queued", "running"] as const)("preserves a canonical %s upload cancellation without synthesizing failure", async (initialState) => {
    vi.mocked(invoke).mockResolvedValue("transfer-1");
    const progress = vi.fn();
    const upload = new TauriTerminalTransferClient().start(scope, "/local/a", "a", {
      collision: "fail", largeUploadConfirmed: false, imagePng: false,
    }, progress);
    channels.at(-1)!.onmessage?.(event({ state: initialState }));
    channels.at(-1)!.onmessage?.(event({
      state: "cancelled", outcome: "notPublished", terminal: true, cleanupStatus: "removed", error: "cancelled by user",
    }));
    await expect(upload).rejects.toThrow("cancelled by user");
    expect(progress).toHaveBeenLastCalledWith(expect.objectContaining({
      state: "cancelled", outcome: "notPublished", cleanupStatus: "removed",
    }));
    expect(progress.mock.calls.at(-1)?.[0]).not.toHaveProperty("failureKind");
  });

  it("inspects local terminal paths without invoking upload preflight", async () => {
    vi.mocked(invoke).mockResolvedValue([
      { path: "/local/a", sizeBytes: "9007199254740993", name: "a" },
      { path: "/local/b", sizeBytes: "2", name: "b" },
    ]);
    await expect(new TauriTerminalTransferClient().inspectLocalPaths(["/local/a", "/local/b"])).resolves.toEqual([
      { path: "/local/a", sizeBytes: "9007199254740993", name: "a" },
      { path: "/local/b", sizeBytes: "2", name: "b" },
    ]);
    expect(invoke).toHaveBeenCalledWith("inspect_local_terminal_paths", { paths: ["/local/a", "/local/b"] });
  });
});
