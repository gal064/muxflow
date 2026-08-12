import { describe, expect, it, vi } from "vitest";
import {
  LARGE_UPLOAD_BYTES,
  MAX_CLIPBOARD_PNG_BYTES,
  MAX_CLIPBOARD_IMAGE_PIXELS,
  assertClipboardPngSize,
  encodeImageAsPng,
  joinShellEscapedPaths,
  parseFileUriList,
  parseCopiedFileList,
  requiresLargeUploadConfirmation,
  sameTerminalTransferScope,
  shellEscapePath,
  supportedClipboardImageType,
  uploadInOriginalOrder,
  validateAgentImagePath,
  validateLocalTerminalPathInspection,
  validateClipboardImageSource,
  validateUploadPreflight,
  type UploadPreflight,
} from "./terminalTransfers";

const preflight = (overrides: Partial<UploadPreflight> = {}): UploadPreflight => ({
  sourcePath: "/home/user/a", name: "a", sizeBytes: "1", sourceKind: "regularFile", readable: true,
  collision: false, ...overrides,
});

describe("terminal path preparation", () => {
  it("POSIX-escapes whitespace, apostrophes, newlines, leading dashes, and Unicode", () => {
    expect(shellEscapePath("two words")).toBe("'two words'");
    expect(shellEscapePath("it's\nhere")).toBe("'it'\"'\"'s\nhere'");
    expect(shellEscapePath("-rf")).toBe("'./-rf'");
    expect(shellEscapePath("/tmp/雪 ☃")).toBe("'/tmp/雪 ☃'");
    expect(joinShellEscapedPaths(["a b", "c'd"])).toBe("'a b' 'c'\"'\"'d'");
  });

  it("never adds bracketed-paste control sequences or Enter", () => {
    const value = joinShellEscapedPaths(["/tmp/a"]);
    expect(value).not.toContain("\u001b[200~");
    expect(value).not.toContain("\u001b[201~");
    expect(value).not.toMatch(/[\r\n]$/u);
  });

  it("rejects invalid normal paths and validates strict raw agent image paths", () => {
    expect(() => shellEscapePath("")).toThrow("empty");
    expect(validateAgentImagePath("/home/user/.cache/app/abc.png")).toBe("/home/user/.cache/app/abc.png");
    expect(() => validateAgentImagePath("/tmp/image one.png")).toThrow("agent-compatible");
    expect(() => validateAgentImagePath("relative.png")).toThrow("agent-compatible");
  });

  it("parses only local file URI clipboard payloads in their original order", () => {
    expect(parseFileUriList("# copied files\nfile:///tmp/a%20b\r\nfile://localhost/tmp/%E9%9B%AA"))
      .toEqual(["/tmp/a b", "/tmp/雪"]);
    expect(() => parseFileUriList("https://example.test/file")).toThrow("Only local file");
    expect(() => parseFileUriList("file://other-host/tmp/a")).toThrow("Only local file");
    expect(parseCopiedFileList("copy\nfile:///tmp/one\nfile:///tmp/two")).toEqual(["/tmp/one", "/tmp/two"]);
  });

  it("accepts only ordered absolute local inspection results with u64-safe sizes", () => {
    expect(validateLocalTerminalPathInspection(
      { path: "/tmp/a", name: "a", sizeBytes: "9007199254740993" },
      "/tmp/a",
    ).sizeBytes).toBe("9007199254740993");
    expect(() => validateLocalTerminalPathInspection({ path: "relative", name: "relative", sizeBytes: "1" }, "relative"))
      .toThrow("absolute");
    expect(() => validateLocalTerminalPathInspection({ path: "/tmp/b", name: "b", sizeBytes: "1" }, "/tmp/a"))
      .toThrow("did not match");
  });
});

describe("terminal upload policy", () => {
  it("compares every terminal render scope field exactly", () => {
    const captured = {
      clientId: "client", hostProfileId: "profile", serverIdentity: "server", connectionEpoch: "7",
      mode: "ssh" as const, paneId: "%1", renderLifetime: "render-1",
    };
    expect(sameTerminalTransferScope(captured, { ...captured })).toBe(true);
    for (const changed of [
      { clientId: "other" }, { hostProfileId: "other" }, { serverIdentity: "other" },
      { connectionEpoch: "8" }, { mode: "local" as const }, { paneId: "%2" }, { renderLifetime: "render-2" },
    ]) expect(sameTerminalTransferScope(captured, { ...captured, ...changed })).toBe(false);
  });

  it("uses exact BigInt boundaries for 500 MiB confirmation and 25 MiB PNG rejection", () => {
    expect(requiresLargeUploadConfirmation([preflight({ sizeBytes: LARGE_UPLOAD_BYTES.toString() })])).toBe(false);
    expect(requiresLargeUploadConfirmation([preflight({ sizeBytes: (LARGE_UPLOAD_BYTES + 1n).toString() })])).toBe(true);
    expect(() => assertClipboardPngSize(MAX_CLIPBOARD_PNG_BYTES)).not.toThrow();
    expect(() => assertClipboardPngSize(MAX_CLIPBOARD_PNG_BYTES + 1n)).toThrow("25 MiB");
  });

  it("accepts bounded PNG, JPEG, and WebP headers before decoding", async () => {
    await expect(validateClipboardImageSource(new Blob([pngHeader(4096, 4096)], { type: "image/png" })))
      .resolves.toEqual({ width: 4096, height: 4096 });
    await expect(validateClipboardImageSource(new Blob([jpegHeader(320, 200)], { type: "image/jpeg" })))
      .resolves.toEqual({ width: 320, height: 200 });
    await expect(validateClipboardImageSource(new Blob([webpHeader(640, 480)], { type: "image/webp" })))
      .resolves.toEqual({ width: 640, height: 480 });
    expect(supportedClipboardImageType(["text/html", "image/webp", "text/plain"])).toBe("image/webp");
  });

  it("rejects source-size and decompression-bomb dimensions before createImageBitmap or canvas", async () => {
    const decode = vi.fn();
    Object.assign(globalThis, { createImageBitmap: decode });
    const overLimit = new Blob([pngHeader(1, 1), new Uint8Array(Number(MAX_CLIPBOARD_PNG_BYTES) - 23)], { type: "image/png" });
    await expect(encodeImageAsPng(overLimit)).rejects.toThrow("source image is larger than the 25 MiB");
    await expect(encodeImageAsPng(new Blob([pngHeader(MAX_CLIPBOARD_IMAGE_PIXELS, 2)], { type: "image/png" })))
      .rejects.toThrow("dimensions exceed");
    expect(decode).not.toHaveBeenCalled();
  }, 15_000);

  it("admits a near-25 MiB source while bounding decoded pixels before canvas allocation", async () => {
    const source = new Blob([pngHeader(4096, 4096), new Uint8Array(Number(MAX_CLIPBOARD_PNG_BYTES) - 25)], { type: "image/png" });
    expect(source.size).toBe(Number(MAX_CLIPBOARD_PNG_BYTES) - 1);
    await expect(validateClipboardImageSource(source)).resolves.toEqual({ width: 4096, height: 4096 });
  });

  it("rejects directories, unreadable, missing/other sources, and malformed u64 sizes", () => {
    expect(() => validateUploadPreflight(preflight({ sourceKind: "directory" }))).toThrow("Directories");
    expect(() => validateUploadPreflight(preflight({ readable: false }))).toThrow("not readable");
    expect(() => validateUploadPreflight(preflight({ sourceKind: "other" }))).toThrow("regular files");
    expect(() => validateUploadPreflight(preflight({ sizeBytes: "9007199254740993.0" }))).toThrow("Invalid source size");
  });

  it("returns verified uploads in source order even when completion order differs", async () => {
    const resolvers: Array<(value: { id: string; destination: string; digest: string }) => void> = [];
    const pending = uploadInOriginalOrder([preflight({ name: "first" }), preflight({ name: "second" })],
      (_item, _index, _progress) => new Promise((resolve) => resolvers.push(resolve)),
      vi.fn(async () => undefined),
      vi.fn());
    resolvers[1]({ id: "2", destination: "/remote/second", digest: "b" });
    resolvers[0]({ id: "1", destination: "/remote/first", digest: "a" });
    await expect(pending).resolves.toEqual([
      { id: "1", destination: "/remote/first", digest: "a" },
      { id: "2", destination: "/remote/second", digest: "b" },
    ]);
  });

  it("cancels nonterminal siblings and awaits their cleanup event before rejecting a batch", async () => {
    const reports: Array<(progress: import("./terminalTransfers").TerminalTransferProgress) => void> = [];
    const rejecters: Array<(reason: Error) => void> = [];
    const starts = ["first", "second"].map((name) => preflight({ sourcePath: `/tmp/${name}`, name }));
    const cancel = vi.fn(async () => undefined);
    let rejected = false;
    const pending = uploadInOriginalOrder(starts, (item, index, report) => {
      reports[index] = report;
      report({ id: `transfer-${index}`, sourcePath: item.sourcePath, name: item.name, state: "running", completedBytes: "1" });
      return new Promise((_resolve, reject) => { rejecters[index] = reject; });
    }, cancel, vi.fn()).catch((error) => { rejected = true; throw error; });

    reports[0]({ id: "transfer-0", sourcePath: "/tmp/first", name: "first", state: "failed", outcome: "notPublished", completedBytes: "1", cleanupStatus: "failed" });
    rejecters[0](new Error("first failed"));
    await Promise.resolve(); await Promise.resolve();
    expect(cancel).toHaveBeenCalledWith("transfer-1");
    expect(rejected).toBe(false);

    reports[1]({ id: "transfer-1", sourcePath: "/tmp/second", name: "second", state: "cancelled", outcome: "notPublished", completedBytes: "1", cleanupStatus: "connectionClosed" });
    rejecters[1](new Error("Upload cancelled."));
    await expect(pending).rejects.toThrow("first failed");
    expect(rejected).toBe(true);
  });

  it("waits for an aborted batch's terminal event and rejects even a late verified resolver", async () => {
    const controller = new AbortController();
    let report!: (progress: import("./terminalTransfers").TerminalTransferProgress) => void;
    let resolve!: (value: { id: string; destination: string; digest: string }) => void;
    const cancel = vi.fn(async () => undefined);
    let settled = false;
    const pending = uploadInOriginalOrder([preflight()], (_item, _index, onProgress) => {
      report = onProgress;
      report({ id: "transfer", sourcePath: "/home/user/a", name: "a", state: "running", completedBytes: "1" });
      return new Promise((done) => { resolve = done; });
    }, cancel, vi.fn(), controller.signal).finally(() => { settled = true; });
    controller.abort();
    await Promise.resolve(); await Promise.resolve();
    expect(cancel).toHaveBeenCalledWith("transfer");
    expect(settled).toBe(false);
    report({ id: "transfer", sourcePath: "/home/user/a", name: "a", state: "completed", outcome: "published", completedBytes: "1" });
    resolve({ id: "transfer", destination: "/remote/stale", digest: "verified" });
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(settled).toBe(true);
  });
});

function pngHeader(width: number, height: number): Uint8Array<ArrayBuffer> {
  const bytes = Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 0, 0, 0, 0, 0);
  const view = new DataView(bytes.buffer);
  view.setUint32(16, width, false);
  view.setUint32(20, height, false);
  return bytes;
}

function jpegHeader(width: number, height: number): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(21);
  bytes.set([0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, height >> 8, height & 0xff, width >> 8, width & 0xff]);
  return bytes;
}

function webpHeader(width: number, height: number): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(30);
  bytes.set(new TextEncoder().encode("RIFF"), 0);
  bytes.set(new TextEncoder().encode("WEBPVP8X"), 8);
  for (const [offset, value] of [[24, width - 1], [27, height - 1]] as const) {
    bytes[offset] = value & 0xff;
    bytes[offset + 1] = (value >> 8) & 0xff;
    bytes[offset + 2] = (value >> 16) & 0xff;
  }
  return bytes;
}
