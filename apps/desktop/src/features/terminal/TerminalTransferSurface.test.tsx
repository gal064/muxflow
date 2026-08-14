import { StrictMode } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TerminalTransferClient, TerminalTransferProgress, TerminalTransferScope, UploadCollisionPolicy, UploadPreflight, VerifiedTerminalUpload } from "./terminalTransfers";
import { TerminalTransferSurface, formatBytes, pointIsInside, progressPercent, type TerminalTransferSurfaceController } from "./TerminalTransferSurface";

const nativeDrag = vi.hoisted(() => ({ handler: undefined as ((event: { payload: Record<string, unknown> }) => void) | undefined }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: vi.fn(async (handler: (event: { payload: Record<string, unknown> }) => void) => {
      nativeDrag.handler = handler;
      return vi.fn();
    }),
  }),
}));
vi.mock("../../commands/useModalDialog", () => ({ useModalDialog: () => ({ current: null }) }));

const scope = (mode: "local" | "ssh"): TerminalTransferScope => ({
  clientId: "client", hostProfileId: "profile", serverIdentity: "server", connectionEpoch: "7", mode,
  paneId: "%1", renderLifetime: "render-1",
});

function item(path: string): UploadPreflight {
  return { sourcePath: path, name: path.split("/").at(-1)!, sizeBytes: "12", sourceKind: "regularFile", readable: true, collision: false };
}

function client(overrides: Partial<TerminalTransferClient> = {}): TerminalTransferClient {
  return {
    inspectLocalPaths: vi.fn(async (paths: readonly string[]) => paths.map((path) => ({ path, name: path.split("/").at(-1)!, sizeBytes: "12" }))),
    preflight: vi.fn(async (_scope, path) => item(path)),
    start: vi.fn(async (_scope, path) => ({ id: path, destination: `/remote/${path.split("/").at(-1)}`, digest: "verified" })),
    cancel: vi.fn(async () => ({ disposition: "cancelRequested" as const, phase: "running" as const })),
    stageClipboardPng: vi.fn(async () => ({ path: "/home/user/.cache/app/image.png", sizeBytes: "10", name: "clipboard-00000000-0000-4000-8000-000000000001.png" })),
    ...overrides,
  };
}

async function mounted(mode: "local" | "ssh", transferClient: TerminalTransferClient, onPaste = vi.fn(), onController?: (value: TerminalTransferSurfaceController | undefined) => void, targetCurrent: HTMLElement | null = null) {
  let renderer: ReactTestRenderer;
  await act(async () => {
    renderer = create(<StrictMode><TerminalTransferSurface client={transferClient} onController={onController} onPaste={onPaste} scope={scope(mode)} target={{ current: targetCurrent }}><div /></TerminalTransferSurface></StrictMode>);
  });
  return { renderer: renderer!, onPaste };
}

async function replaceScope(
  view: Awaited<ReturnType<typeof mounted>>,
  transferClient: TerminalTransferClient,
  nextScope: TerminalTransferScope | undefined,
) {
  await act(async () => {
    view.renderer.update(<StrictMode><TerminalTransferSurface client={transferClient} onPaste={view.onPaste} scope={nextScope} target={{ current: null }}><div /></TerminalTransferSurface></StrictMode>);
    await Promise.resolve();
  });
}

function clipboardEvent(uriList: string) {
  return { preventDefault: vi.fn(), clipboardData: { files: [], getData: (kind: string) => kind === "text/uri-list" ? uriList : "" } };
}

function clipboardFiles(uriList: string, files: File[]) {
  return { preventDefault: vi.fn(), clipboardData: { files, getData: (kind: string) => kind === "text/uri-list" ? uriList : "" } };
}

function pathFile(path: string, type: string): File {
  return Object.assign(new Blob([path], { type }), {
    name: path.split("/").at(-1)!, path, lastModified: 0, webkitRelativePath: "",
  }) as File;
}

function pngImage(width = 2, height = 3): Blob {
  const bytes = Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 0, 0, 0, 0, 0);
  const view = new DataView(bytes.buffer);
  view.setUint32(16, width, false);
  view.setUint32(20, height, false);
  return new Blob([bytes], { type: "image/png" });
}

function jpegImage(width = 2, height = 3): Blob {
  const bytes = new Uint8Array(21);
  bytes.set([0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, height >> 8, height & 0xff, width >> 8, width & 0xff]);
  return new Blob([bytes], { type: "image/jpeg" });
}

describe("TerminalTransferSurface", () => {
  beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); nativeDrag.handler = undefined; });
  afterEach(() => { Reflect.deleteProperty(globalThis, "window"); });

  it("pastes inspected local paths once, escaped, and never adds Enter", async () => {
    const transferClient = client();
    const view = await mounted("local", transferClient);
    const event = clipboardEvent("file:///tmp/a%20b\nfile:///tmp/it%27s");
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(event);
      await Promise.resolve(); await Promise.resolve();
    });
    expect(event.preventDefault).toHaveBeenCalled();
    expect(view.onPaste).toHaveBeenCalledWith("'/tmp/a b' '/tmp/it'\"'\"'s'");
    expect(view.onPaste.mock.calls[0][0]).not.toMatch(/[\r\n]$/u);
    expect(transferClient.inspectLocalPaths).toHaveBeenCalledWith(["/tmp/a b", "/tmp/it's"]);
    expect(transferClient.preflight).not.toHaveBeenCalled();
    expect(transferClient.start).not.toHaveBeenCalled();
  });

  it("pastes a locally inspected file larger than remote staging space without bulk calls", async () => {
    const transferClient = client({
      inspectLocalPaths: vi.fn(async () => [{ path: "/tmp/huge file", name: "huge file", sizeBytes: (5n * 1024n * 1024n * 1024n).toString() }]),
    });
    const view = await mounted("local", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/huge%20file"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).toHaveBeenCalledWith("'/tmp/huge file'");
    expect(transferClient.preflight).not.toHaveBeenCalled();
    expect(transferClient.start).not.toHaveBeenCalled();
    expect(transferClient.cancel).not.toHaveBeenCalled();
  });

  it.each(["local", "ssh"] as const)("treats a copied PNG with URI and image MIME as a %s file path", async (mode) => {
    const transferClient = client();
    const view = await mounted(mode, transferClient);
    const event = clipboardFiles("file:///tmp/copied.png", [pathFile("/tmp/copied.png", "image/png")]);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(event);
      await Promise.resolve(); await Promise.resolve();
    });
    expect(event.preventDefault).toHaveBeenCalled();
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    if (mode === "local") expect(transferClient.inspectLocalPaths).toHaveBeenCalledWith(["/tmp/copied.png"]);
    else expect(transferClient.preflight).toHaveBeenCalledWith(scope("ssh"), "/tmp/copied.png", "copied.png", expect.objectContaining({ imagePng: false }), expect.any(Function), expect.any(AbortSignal));
    expect(view.onPaste).toHaveBeenCalledWith(mode === "local" ? "'/tmp/copied.png'" : "'/remote/copied.png'");
  });

  it.each(["local", "ssh"] as const)("prioritizes Navigator URI lists over image MIME for %s clipboard paste", async (mode) => {
    const transferClient = client();
    let controller: TerminalTransferSurfaceController | undefined;
    Object.defineProperty(globalThis.navigator, "clipboard", { configurable: true, value: { read: vi.fn(async () => [{
      types: ["image/png", "text/uri-list"],
      getType: vi.fn(async (type: string) => type === "text/uri-list"
        ? new Blob(["file:///tmp/copied.png\nfile:///tmp/notes.txt"], { type })
        : new Blob(["image"], { type })),
    }]) } });
    const view = await mounted(mode, transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    const paths = ["/tmp/copied.png", "/tmp/notes.txt"];
    if (mode === "local") expect(transferClient.inspectLocalPaths).toHaveBeenCalledWith(paths);
    else expect(vi.mocked(transferClient.start).mock.calls.map((call) => call[1])).toEqual(paths);
    expect(view.onPaste).toHaveBeenCalledWith(mode === "local"
      ? "'/tmp/copied.png' '/tmp/notes.txt'"
      : "'/remote/copied.png' '/remote/notes.txt'");
  });

  it.each(["local", "ssh"] as const)("uses the native Linux file clipboard before browser image MIME for %s", async (mode) => {
    const transferClient = client({
      readNativeClipboard: vi.fn(async () => ({ kind: "files" as const, uris: ["file:///tmp/copied.png", "file:///tmp/notes%20one.txt"] })),
    });
    let controller: TerminalTransferSurfaceController | undefined;
    const view = await mounted(mode, transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    const paths = ["/tmp/copied.png", "/tmp/notes one.txt"];
    if (mode === "local") expect(transferClient.inspectLocalPaths).toHaveBeenCalledWith(paths);
    else expect(vi.mocked(transferClient.start).mock.calls.map((call) => call[1])).toEqual(paths);
    expect(view.onPaste).toHaveBeenCalledWith(mode === "local"
      ? "'/tmp/copied.png' '/tmp/notes one.txt'"
      : "'/remote/copied.png' '/remote/notes one.txt'");
  });

  it.each(["local", "ssh"] as const)("decodes and shell-escapes a native Linux newline filename for %s without adding Enter", async (mode) => {
    const transferClient = client({
      readNativeClipboard: vi.fn(async () => ({ kind: "files" as const, uris: ["file:///tmp/line%0Abreak.txt"] })),
    });
    let controller: TerminalTransferSurfaceController | undefined;
    const view = await mounted(mode, transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    const path = "/tmp/line\nbreak.txt";
    if (mode === "local") expect(transferClient.inspectLocalPaths).toHaveBeenCalledWith([path]);
    else expect(vi.mocked(transferClient.start).mock.calls.map((call) => call[1])).toEqual([path]);
    expect(view.onPaste).toHaveBeenCalledWith(mode === "local" ? `'${path}'` : "'/remote/line\nbreak.txt'");
    expect(view.onPaste).not.toHaveBeenCalledWith(expect.stringContaining("\r"));
  });

  it.each(["local", "ssh"] as const)("pastes a backend-staged native Linux clipboard image for %s", async (mode) => {
    const name = "clipboard-00000000-0000-4000-8000-000000000001.png";
    const staged = { path: `/cache/${name}`, sizeBytes: "1024", name };
    const transferClient = client({ readNativeClipboard: vi.fn(async () => ({ kind: "image" as const, staged })) });
    let controller: TerminalTransferSurfaceController | undefined;
    const view = await mounted(mode, transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    if (mode === "ssh") expect(transferClient.preflight).toHaveBeenCalledWith(
      scope("ssh"), staged.path, name, expect.objectContaining({ imagePng: true }), expect.any(Function), expect.any(AbortSignal),
    );
    expect(view.onPaste).toHaveBeenCalledWith(mode === "local" ? staged.path : `/remote/${name}`);
  });

  it.each(["local", "ssh"] as const)("pastes native clipboard text copied by another application for %s", async (mode) => {
    // M10-E054: WebKit refuses `navigator.clipboard` reads for foreign content,
    // so the native rung is the only path that carries this text through.
    const transferClient = client({ readNativeClipboard: vi.fn(async () => ({ kind: "text" as const, text: "echo from-another-app\n" })) });
    let controller: TerminalTransferSurfaceController | undefined;
    const view = await mounted(mode, transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    expect(view.onPaste).toHaveBeenCalledWith("echo from-another-app\n");
    expect(transferClient.preflight).not.toHaveBeenCalled();
    expect(transferClient.start).not.toHaveBeenCalled();
  });

  it("renders a native clipboard rejection instead of leaking an unhandled paste promise", async () => {
    const diagnostic = vi.fn();
    const transferClient = client({
      readNativeClipboard: vi.fn(async () => { throw new Error("clipboard PNG exceeds the 25 MiB encoded-image limit"); }),
    });
    let controller: TerminalTransferSurfaceController | undefined;
    let renderer: ReactTestRenderer;
    await act(async () => {
      renderer = create(<TerminalTransferSurface
        client={transferClient}
        onController={(value) => { controller = value; }}
        onDiagnostic={diagnostic}
        onPaste={vi.fn()}
        scope={scope("local")}
        target={{ current: null }}
      ><div /></TerminalTransferSurface>);
    });
    await act(async () => { expect(await controller!.pasteClipboard()).toBe(true); });
    expect(alertText(renderer!.root.findByProps({ role: "alert" }))).toContain("25 MiB");
    expect(diagnostic).toHaveBeenCalledWith(expect.stringContaining("25 MiB"));
  });

  it("processes every mixed PNG/text DOM drop path in original order without image staging", async () => {
    const transferClient = client();
    const view = await mounted("ssh", transferClient);
    const drop = {
      preventDefault: vi.fn(),
      dataTransfer: {
        files: [pathFile("/tmp/first.png", "image/png"), pathFile("/tmp/second.txt", "text/plain")],
        getData: () => "",
      },
    };
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onDrop(drop);
      await Promise.resolve(); await Promise.resolve();
    });
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    expect(vi.mocked(transferClient.start).mock.calls.map((call) => call[1])).toEqual(["/tmp/first.png", "/tmp/second.txt"]);
    expect(view.onPaste).toHaveBeenCalledWith("'/remote/first.png' '/remote/second.txt'");
  });

  it("processes every native Tauri drop path in original order without MIME reinterpretation", async () => {
    Object.assign(globalThis, { window: { __TAURI_INTERNALS__: {}, devicePixelRatio: 1 } });
    const transferClient = client();
    const target = { getBoundingClientRect: () => ({ left: 0, right: 200, top: 0, bottom: 200 }) } as HTMLElement;
    const view = await mounted("ssh", transferClient, vi.fn(), undefined, target);
    await act(async () => {
      nativeDrag.handler?.({ payload: { type: "drop", paths: ["/tmp/first.png", "/tmp/second.txt"], position: { x: 20, y: 20 } } });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(transferClient.stageClipboardPng).not.toHaveBeenCalled();
    expect(vi.mocked(transferClient.start).mock.calls.map((call) => call[1])).toEqual(["/tmp/first.png", "/tmp/second.txt"]);
    expect(view.onPaste).toHaveBeenCalledWith("'/remote/first.png' '/remote/second.txt'");
  });

  it("waits for every verified remote completion and preserves the drop order", async () => {
    const resolves = new Map<string, (value: VerifiedTerminalUpload) => void>();
    const transferClient = client({
      start: vi.fn((_scope: TerminalTransferScope, path: string, _name: string, _options: { collision: UploadCollisionPolicy; largeUploadConfirmed: boolean; imagePng: boolean }, onProgress: (progress: TerminalTransferProgress) => void): Promise<VerifiedTerminalUpload> => {
        onProgress({ id: path, sourcePath: path, name: path, state: "running", completedBytes: "1", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((resolve) => resolves.set(path, resolve));
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/first\nfile:///tmp/second"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(transferClient.start).toHaveBeenCalledTimes(2);
    expect(view.onPaste).not.toHaveBeenCalled();
    await act(async () => {
      resolves.get("/tmp/second")!({ id: "2", destination: "/remote/second", digest: "b" });
      await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
    await act(async () => {
      resolves.get("/tmp/first")!({ id: "1", destination: "/remote/first", digest: "a" });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).toHaveBeenCalledTimes(1);
    expect(view.onPaste).toHaveBeenCalledWith("'/remote/first' '/remote/second'");
  });

  it("cancels and settles sibling uploads after one failure without pasting a partial batch", async () => {
    const reports = new Map<string, (progress: TerminalTransferProgress) => void>();
    const rejecters = new Map<string, (reason: Error) => void>();
    const transferClient = client({
      start: vi.fn((_scope: TerminalTransferScope, path: string, _name: string, _options: { collision: UploadCollisionPolicy; largeUploadConfirmed: boolean; imagePng: boolean }, onProgress: (progress: TerminalTransferProgress) => void): Promise<VerifiedTerminalUpload> => {
        const id = `transfer-${path.split("/").at(-1)}`;
        reports.set(id, onProgress);
        onProgress({ id, sourcePath: path, name: path, state: "running", completedBytes: "1", totalBytes: "12" });
        return new Promise((_resolve, reject) => rejecters.set(id, reject));
      }),
      cancel: vi.fn(async (id) => {
        reports.get(id)?.({ id, sourcePath: "/tmp/second", name: "second", state: "cancelled", outcome: "notPublished", completedBytes: "1", cleanupStatus: "connectionClosed" });
        rejecters.get(id)?.(new Error("Upload cancelled."));
        return { disposition: "cancelRequested" as const, phase: "running" as const };
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/first\nfile:///tmp/second"));
      await Promise.resolve(); await Promise.resolve();
    });
    await act(async () => {
      reports.get("transfer-first")!({ id: "transfer-first", sourcePath: "/tmp/first", name: "first", state: "failed", outcome: "notPublished", failureKind: "cleanup", completedBytes: "1", cleanupStatus: "failed", cleanupError: "permission denied", error: "disk full" });
      rejecters.get("transfer-first")!(new Error("disk full"));
      await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
    });
    expect(transferClient.cancel).toHaveBeenCalledWith("transfer-second");
    expect(view.onPaste).not.toHaveBeenCalled();
    expect(view.renderer.root.findAllByProps({ role: "alert" }).some((node) => alertText(node).includes("disk full"))).toBe(true);
    expect(view.renderer.root.findAllByProps({ role: "alert" }).some((node) => alertText(node).includes("Partial cleanup failed: permission denied"))).toBe(true);
  });

  it("surfaces source rejection and never starts or pastes", async () => {
    const transferClient = client({ preflight: vi.fn(async () => { throw new Error("Directories cannot be dropped into terminals"); }) });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/folder"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(alertText(view.renderer.root.findByProps({ role: "alert" }))).toContain("Directories");
    expect(transferClient.start).not.toHaveBeenCalled();
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("explains an independently timed-out upload with unknown publication outcome", async () => {
    const transferClient = client({
      start: vi.fn(async (_scope, path, _name, _options, onProgress) => {
        onProgress({
          id: "timeout", sourcePath: path, name: "timeout", state: "failed", outcome: "unknown",
          failureKind: "timeout", completedBytes: "12", error: "reconciliation deadline expired",
        });
        throw new Error("reconciliation deadline expired");
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/timeout"));
      await Promise.resolve(); await Promise.resolve();
    });
    const alerts = view.renderer.root.findAllByProps({ role: "alert" }).map((node) => alertText(node));
    expect(alerts).toContain("Upload timed out before an authoritative result arrived.");
    expect(alerts).toContain("The upload outcome is unknown. Inspect the destination before retrying or pasting.");
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("requires an explicit large/collision review and forwards the reviewed overwrite policy", async () => {
    const transferClient = client({
      preflight: vi.fn(async (_scope, path) => ({ ...item(path), sizeBytes: "524288001", collision: true })),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/large"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.renderer.root.findByProps({ role: "dialog" })).toBeDefined();
    expect(view.renderer.root.findAllByProps({ role: "alert" }).some((node) => alertText(node).includes("500 MiB"))).toBe(true);
    await act(async () => {
      view.renderer.root.findByProps({ "aria-label": "Upload collision behavior" }).props.onChange({ target: { value: "overwriteConfirmed" } });
    });
    await act(async () => {
      view.renderer.root.findByProps({ role: "dialog" }).props.onSubmit({ preventDefault: vi.fn() });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(transferClient.start).toHaveBeenCalledWith(
      scope("ssh"), "/tmp/large", "large",
      { collision: "overwriteConfirmed", largeUploadConfirmed: true, imagePng: false },
      expect.any(Function),
    );
    expect(view.onPaste).toHaveBeenCalledWith("'/remote/large'");
  });

  it.each(["queued", "running"] as const)("routes accessible cancel actions for a %s upload to the exact backend transfer", async (state) => {
    const transferClient = client({
      start: vi.fn(async (_scope, path, _name, _options, onProgress) => {
        onProgress({ id: "transfer-7", sourcePath: path, name: "waiting", state, completedBytes: "0", totalBytes: "10" });
        return new Promise<VerifiedTerminalUpload>(() => undefined);
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/waiting"));
      await Promise.resolve(); await Promise.resolve();
    });
    await act(async () => view.renderer.root.findByProps({ "aria-label": "Cancel upload waiting" }).props.onClick());
    expect(transferClient.cancel).toHaveBeenCalledWith("transfer-7");
  });

  it.each([
    ["client", { clientId: "replacement-client" }],
    ["profile", { hostProfileId: "replacement-profile" }],
    ["server replacement", { serverIdentity: "replacement-server" }],
    ["connection epoch", { connectionEpoch: "8" }],
    ["pane", { paneId: "%2" }],
    ["render lifetime", { renderLifetime: "render-2" }],
  ])("cancels queued uploads and rejects stale completion after a %s scope change", async (_label, changed) => {
    let report!: (progress: TerminalTransferProgress) => void;
    let resolve!: (value: VerifiedTerminalUpload) => void;
    const transferClient = client({
      start: vi.fn((_scope, path, _name, _options, onProgress) => {
        report = onProgress;
        onProgress({ id: "queued-1", sourcePath: path, name: "queued", state: "queued", completedBytes: "0", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((done) => { resolve = done; });
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/queued"));
      await Promise.resolve(); await Promise.resolve();
    });
    const replacement = { ...scope("ssh"), ...changed };
    await replaceScope(view, transferClient, replacement);
    expect(transferClient.cancel).toHaveBeenCalledWith("queued-1");

    await act(async () => {
      report({ id: "queued-1", sourcePath: "/tmp/queued", name: "queued", state: "completed", outcome: "published", completedBytes: "12", totalBytes: "12", destination: "/remote/stale", digest: "verified" });
      resolve({ id: "queued-1", destination: "/remote/stale", digest: "verified" });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
    expect(view.renderer.root.findByProps({ "aria-label": "Upload queued: Completed" })).toBeDefined();
    expect(JSON.stringify(view.renderer.toJSON())).toContain("%1");
  });

  it("disconnects a streaming batch, waits for its terminal cancellation, and never pastes", async () => {
    let report!: (progress: TerminalTransferProgress) => void;
    let reject!: (reason: Error) => void;
    const transferClient = client({
      start: vi.fn((_scope, path, _name, _options, onProgress) => {
        report = onProgress;
        onProgress({ id: "stream-1", sourcePath: path, name: "stream", state: "running", completedBytes: "3", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((_resolve, fail) => { reject = fail; });
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/stream"));
      await Promise.resolve(); await Promise.resolve();
    });
    await replaceScope(view, transferClient, undefined);
    expect(transferClient.cancel).toHaveBeenCalledWith("stream-1");
    expect(view.onPaste).not.toHaveBeenCalled();

    await act(async () => {
      report({ id: "stream-1", sourcePath: "/tmp/stream", name: "stream", state: "cancelled", outcome: "notPublished", completedBytes: "3", totalBytes: "12", cleanupStatus: "removed" });
      reject(new Error("Upload cancelled."));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("cancels a verifying upload on server replacement and waits for its terminal state", async () => {
    let report!: (progress: TerminalTransferProgress) => void;
    let reject!: (reason: Error) => void;
    const transferClient = client({
      start: vi.fn((_scope, path, _name, _options, onProgress) => {
        report = onProgress;
        onProgress({ id: "precommit-1", sourcePath: path, name: "precommit", state: "verifying", completedBytes: "12", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((_resolve, fail) => { reject = fail; });
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/precommit"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.renderer.root.findAllByProps({ "aria-label": "Cancel upload precommit" })).toHaveLength(0);
    expect(JSON.stringify(view.renderer.toJSON())).toContain("Commit in progress");
    await replaceScope(view, transferClient, { ...scope("ssh"), serverIdentity: "new-server", connectionEpoch: "8" });
    expect(transferClient.cancel).toHaveBeenCalledWith("precommit-1");
    await act(async () => {
      report({ id: "precommit-1", sourcePath: "/tmp/precommit", name: "precommit", state: "cancelled", outcome: "notPublished", completedBytes: "12", totalBytes: "12", cleanupStatus: "removed" });
      reject(new Error("Upload cancelled."));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("rejects a verified completion racing a server replacement before final paste", async () => {
    let resolve!: (value: VerifiedTerminalUpload) => void;
    const transferClient = client({
      start: vi.fn((_scope, path, _name, _options, onProgress) => {
        onProgress({ id: "precommit-1", sourcePath: path, name: "precommit", state: "verifying", completedBytes: "12", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((done) => { resolve = done; });
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/precommit"));
      await Promise.resolve(); await Promise.resolve();
    });
    await act(async () => {
      resolve({ id: "precommit-1", destination: "/remote/committed-stale", digest: "verified" });
      view.renderer.update(<StrictMode><TerminalTransferSurface client={transferClient} onPaste={view.onPaste} scope={{ ...scope("ssh"), serverIdentity: "new-server", connectionEpoch: "8" }} target={{ current: null }}><div /></TerminalTransferSurface></StrictMode>);
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("cancels known work on unmount and ignores every late resolver", async () => {
    let report!: (progress: TerminalTransferProgress) => void;
    let resolve!: (value: VerifiedTerminalUpload) => void;
    const transferClient = client({
      start: vi.fn((_scope, path, _name, _options, onProgress) => {
        report = onProgress;
        onProgress({ id: "unmount-1", sourcePath: path, name: "unmount", state: "running", completedBytes: "1", totalBytes: "12" });
        return new Promise<VerifiedTerminalUpload>((done) => { resolve = done; });
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/unmount"));
      await Promise.resolve(); await Promise.resolve();
      view.renderer.unmount();
      await Promise.resolve();
    });
    expect(transferClient.cancel).toHaveBeenCalledWith("unmount-1");
    await act(async () => {
      report({ id: "unmount-1", sourcePath: "/tmp/unmount", name: "unmount", state: "completed", outcome: "published", completedBytes: "12", destination: "/remote/stale", digest: "verified" });
      resolve({ id: "unmount-1", destination: "/remote/stale", digest: "verified" });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
  });

  it("encodes a real clipboard image as PNG and pastes the validated staged path raw", async () => {
    const close = vi.fn();
    Object.assign(globalThis, {
      createImageBitmap: vi.fn(async () => ({ width: 2, height: 3, close })),
      document: {
        createElement: () => ({
          width: 0, height: 0,
          getContext: () => ({ drawImage: vi.fn() }),
          toBlob: (callback: (blob: Blob) => void) => callback(new Blob([Uint8Array.of(137, 80, 78, 71)], { type: "image/png" })),
        }),
      },
    });
    const transferClient = client();
    let controller: TerminalTransferSurfaceController | undefined;
    Object.defineProperty(globalThis.navigator, "clipboard", { configurable: true, value: { read: vi.fn(async () => [{
      types: ["text/html", "image/jpeg", "text/plain"], getType: vi.fn(async (type: string) => type === "image/jpeg" ? jpegImage() : new Blob(["alternative"], { type })),
    }]) } });
    const view = await mounted("local", transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => {
      await controller!.pasteClipboard();
    });
    expect(transferClient.stageClipboardPng).toHaveBeenCalledWith(expect.any(Uint8Array));
    expect(view.onPaste).toHaveBeenCalledWith("/home/user/.cache/app/image.png");
    expect(view.onPaste).not.toHaveBeenCalledWith(expect.stringContaining("'"));
    expect(close).toHaveBeenCalled();
  });

  it("accepts a Linux WebKit-style DOM image paste with HTML and text alternatives", async () => {
    Object.assign(globalThis, {
      createImageBitmap: vi.fn(async () => ({ width: 2, height: 3, close: vi.fn() })),
      document: {
        createElement: () => ({
          width: 0, height: 0, getContext: () => ({ drawImage: vi.fn() }),
          toBlob: (callback: (blob: Blob) => void) => callback(pngImage()),
        }),
      },
    });
    const transferClient = client();
    const view = await mounted("local", transferClient);
    const image = Object.assign(pngImage(), { name: "clipboard.png", lastModified: 0, webkitRelativePath: "" }) as File;
    const event = {
      preventDefault: vi.fn(),
      clipboardData: {
        files: [],
        items: [
          { kind: "string", type: "text/html" },
          { kind: "file", type: "image/png", getAsFile: () => image },
          { kind: "string", type: "text/plain" },
        ],
        getData: () => "",
      },
    };
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(event);
      await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
    });
    expect(event.preventDefault).toHaveBeenCalled();
    expect(transferClient.stageClipboardPng).toHaveBeenCalledWith(expect.any(Uint8Array));
    expect(view.onPaste).toHaveBeenCalledWith("/home/user/.cache/app/image.png");
  });

  it("accepts two successive remote clipboard images with distinct backend-generated destinations", async () => {
    const names = [
      "clipboard-00000000-0000-4000-8000-000000000001.png",
      "clipboard-00000000-0000-4000-8000-000000000002.png",
    ];
    Object.assign(globalThis, {
      createImageBitmap: vi.fn(async () => ({ width: 1, height: 1, close: vi.fn() })),
      document: {
        createElement: () => ({
          width: 0, height: 0, getContext: () => ({ drawImage: vi.fn() }),
          toBlob: (callback: (blob: Blob) => void) => callback(new Blob([Uint8Array.of(137, 80, 78, 71)], { type: "image/png" })),
        }),
      },
    });
    const transferClient = client({
      stageClipboardPng: vi.fn(async () => {
        const name = names.shift()!;
        return { path: `/cache/${name}`, sizeBytes: "4", name };
      }),
      preflight: vi.fn(async (_scope, path, destinationName) => ({ ...item(path), name: destinationName })),
      start: vi.fn(async (_scope, _path, destinationName) => ({ id: destinationName, destination: `/remote/${destinationName}`, digest: "verified" })),
    });
    let controller: TerminalTransferSurfaceController | undefined;
    Object.defineProperty(globalThis.navigator, "clipboard", { configurable: true, value: { read: vi.fn(async () => [{
      types: ["image/png"], getType: vi.fn(async () => pngImage(1, 1)),
    }]) } });
    const view = await mounted("ssh", transferClient, vi.fn(), (value) => { controller = value; });
    await act(async () => { await controller!.pasteClipboard(); });
    await act(async () => { await controller!.pasteClipboard(); });
    expect(view.onPaste.mock.calls.map(([value]) => value)).toEqual([
      "/remote/clipboard-00000000-0000-4000-8000-000000000001.png",
      "/remote/clipboard-00000000-0000-4000-8000-000000000002.png",
    ]);
    expect(view.onPaste.mock.calls.flat().join(" ")).not.toContain("clipboard (1).png");
  });

  it("aborts a silent preflight when pane lifetime is replaced and never starts or pastes", async () => {
    let capturedSignal: AbortSignal | undefined;
    const transferClient = client({
      preflight: vi.fn((_scope, _path, _name, _options, _progress, signal) => {
        capturedSignal = signal;
        return new Promise<UploadPreflight>((_resolve, reject) => signal?.addEventListener("abort", () => reject(new DOMException("cancelled", "AbortError")), { once: true }));
      }),
    });
    const view = await mounted("ssh", transferClient);
    await act(async () => {
      view.renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardEvent("file:///tmp/silent"));
      await Promise.resolve();
    });
    expect(capturedSignal?.aborted).toBe(false);
    await replaceScope(view, transferClient, { ...scope("ssh"), renderLifetime: "render-2" });
    expect(capturedSignal?.aborted).toBe(true);
    expect(transferClient.start).not.toHaveBeenCalled();
    expect(view.onPaste).not.toHaveBeenCalled();
  });
});

describe("u64-safe transfer presentation", () => {
  it("formats and calculates beyond Number.MAX_SAFE_INTEGER using BigInt", () => {
    expect(formatBytes("9007199254740993")).toMatch(/PiB|TiB/u);
    expect(progressPercent("9007199254740993", "18014398509481986")).toBe(50);
  });

  it("maps native physical coordinates to CSS pixels", () => {
    const element = { getBoundingClientRect: () => ({ left: 10, right: 110, top: 20, bottom: 120 }) };
    expect(pointIsInside(element as HTMLElement, { x: 100, y: 120 }, 2)).toBe(true);
    expect(pointIsInside(element as HTMLElement, { x: 5, y: 5 }, 2)).toBe(false);
  });
});

/**
 * A rejection banner is a summary plus a `<details>` disclosure now, so its
 * text no longer sits directly under the alert node.
 */
function alertText(node: { children: readonly unknown[] }): string {
  const walk = (value: unknown): string => {
    if (typeof value === "string") return value;
    if (Array.isArray(value)) return value.map(walk).join("");
    if (value && typeof value === "object" && "children" in value) return walk((value as { children: unknown }).children ?? []);
    return "";
  };
  return walk(node.children);
}
