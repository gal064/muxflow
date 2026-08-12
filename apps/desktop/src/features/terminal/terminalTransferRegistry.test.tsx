// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { TerminalTransferHistory, TerminalTransferSurface } from "./TerminalTransferSurface";
import { terminalTransferRecordKey, useTerminalTransferRegistry, type TerminalTransferRegistry } from "./terminalTransferRegistry";
import type { TerminalTransferClient, TerminalTransferProgress, TerminalTransferScope, UploadPreflight, VerifiedTerminalUpload } from "./terminalTransfers";

vi.mock("@tauri-apps/api/webview", () => ({ getCurrentWebview: () => ({ onDragDropEvent: vi.fn() }) }));
vi.mock("../../commands/useModalDialog", () => ({ useModalDialog: () => ({ current: null }) }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const originalScope: TerminalTransferScope = {
  clientId: "client", hostProfileId: "profile", serverIdentity: "old-server", connectionEpoch: "7", mode: "ssh",
  paneId: "%1", renderLifetime: "render-1",
};

const preflight: UploadPreflight = {
  sourcePath: "/tmp/image", name: "image", sizeBytes: "12", sourceKind: "regularFile", readable: true, collision: false,
};

function clipboardPaste() {
  return { preventDefault: vi.fn(), clipboardData: { files: [], getData: (type: string) => type === "text/uri-list" ? "file:///tmp/image" : "" } };
}

function Harness({ client, onPaste, onRegistry, scope, showSurface }: {
  client: TerminalTransferClient;
  onPaste(value: string): void;
  onRegistry?(registry: TerminalTransferRegistry): void;
  scope: TerminalTransferScope;
  showSurface: boolean;
}) {
  const registry = useTerminalTransferRegistry();
  onRegistry?.(registry);
  return <>
    {showSurface && <TerminalTransferSurface client={client} onPaste={onPaste} registry={registry} scope={scope} target={{ current: null }}><div /></TerminalTransferSurface>}
    <TerminalTransferHistory client={client} registry={registry} />
  </>;
}

async function verifyingHarness() {
  let report!: (progress: TerminalTransferProgress) => void;
  let resolve!: (value: VerifiedTerminalUpload) => void;
  let reject!: (reason: Error) => void;
  const client: TerminalTransferClient = {
    inspectLocalPaths: vi.fn(),
    preflight: vi.fn(async () => preflight),
    start: vi.fn((_scope, _path, _name, _options, onProgress) => {
      report = onProgress;
      onProgress({ id: "transfer", sourcePath: "/tmp/image", name: "image", state: "verifying", completedBytes: "12", totalBytes: "12" });
      return new Promise<VerifiedTerminalUpload>((done, fail) => { resolve = done; reject = fail; });
    }),
    cancel: vi.fn(async () => ({ disposition: "awaitingAuthoritativeOutcome" as const, phase: "verifying" as const })),
    stageClipboardPng: vi.fn(),
  };
  const onPaste = vi.fn();
  let registry!: TerminalTransferRegistry;
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness client={client} onPaste={onPaste} onRegistry={(value) => { registry = value; }} scope={originalScope} showSurface />); });
  await act(async () => {
    renderer.root.findByProps({ className: "terminal-transfer-surface" }).props.onPasteCapture(clipboardPaste());
    await Promise.resolve(); await Promise.resolve();
  });
  expect(renderer.root.findByProps({ "aria-label": "Upload image: Verifying and committing" })).toBeDefined();
  return { client, onPaste, registry, reject, renderer, report, resolve };
}

describe("app-scoped terminal transfer registry", () => {
  it("keys ownership by transfer and every captured connection/pane identity field", () => {
    const original = terminalTransferRecordKey(originalScope, "transfer");
    for (const changed of [
      { clientId: "other-client" }, { hostProfileId: "other-profile" }, { serverIdentity: "other-server" },
      { connectionEpoch: "8" }, { paneId: "%2" },
    ]) expect(terminalTransferRecordKey({ ...originalScope, ...changed }, "transfer")).not.toBe(original);
    expect(terminalTransferRecordKey(originalScope, "other-transfer")).not.toBe(original);
    expect(terminalTransferRecordKey({ ...originalScope, renderLifetime: "other-render" }, "transfer")).toBe(original);
  });

  it("retains a published result from the old server after replacement without pasting it", async () => {
    const view = await verifyingHarness();
    await act(async () => { view.renderer.update(<Harness client={view.client} onPaste={view.onPaste} scope={{ ...originalScope, serverIdentity: "new-server", connectionEpoch: "8" }} showSurface />); });
    await act(async () => {
      view.report({ id: "transfer", sourcePath: "/tmp/image", name: "image", state: "completed", outcome: "published", completedBytes: "12", totalBytes: "12", destination: "/remote/image", digest: "verified", cleanupStatus: "removed" });
      view.resolve({ id: "transfer", destination: "/remote/image", digest: "verified" });
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
    expect(view.renderer.root.findByProps({ "aria-label": "Upload image: Completed" })).toBeDefined();
    expect(JSON.stringify(view.renderer.toJSON())).toContain("old-server");
  });

  it("retains an unknown old-scope result after the initiating surface unmounts", async () => {
    const view = await verifyingHarness();
    await act(async () => { view.renderer.update(<Harness client={view.client} onPaste={view.onPaste} scope={originalScope} showSurface={false} />); });
    await act(async () => {
      view.report({ id: "transfer", sourcePath: "/tmp/image", name: "image", state: "failed", outcome: "unknown", failureKind: "outcomeUnknown", completedBytes: "12", cleanupStatus: "retained", cleanupError: "quarantined" });
      view.reject(new Error("outcome unknown"));
      await Promise.resolve(); await Promise.resolve();
    });
    expect(view.onPaste).not.toHaveBeenCalled();
    expect(JSON.stringify(view.renderer.toJSON())).toContain("outcome is unknown");
    expect(JSON.stringify(view.renderer.toJSON())).toContain("quarantined");
  });

  it("retains cleanup failure after unmount and never downgrades it on a later cancelled no-op", async () => {
    const view = await verifyingHarness();
    await act(async () => { view.renderer.update(<Harness client={view.client} onPaste={view.onPaste} scope={originalScope} showSurface={false} />); });
    await act(async () => {
      view.report({
        id: "transfer", sourcePath: "/tmp/image", name: "image", state: "failed", outcome: "notPublished", failureKind: "cleanup",
        completedBytes: "12", cleanupStatus: "failed", cleanupError: "rollback failed",
      });
      view.reject(new Error("cleanup failed"));
      await Promise.resolve(); await Promise.resolve();
    });
    await act(async () => { view.registry.record(originalScope, {
      id: "transfer", sourcePath: "/tmp/image", name: "image", state: "cancelled", outcome: "notPublished",
      completedBytes: "12", cleanupStatus: "removed", error: "already terminal",
    }); });
    expect(view.renderer.root.findByProps({ "aria-label": "Upload image: Failed" })).toBeDefined();
    expect(view.onPaste).not.toHaveBeenCalled();
    const json = JSON.stringify(view.renderer.toJSON());
    expect(json).toContain("rollback failed");
    expect(json).not.toContain("already terminal");
  });
});
