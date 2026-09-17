import { describe, expect, it, vi } from "vitest";
import {
  cancelInternalPathDragSource,
  claimNativeInternalPathDrag,
  consumeNativeInternalPathDrop,
  finishInternalPathDrag,
  INTERNAL_PATH_DRAG_TYPE,
  readInternalPathDrop,
  releaseNativeInternalPathDrag,
  writeInternalPathDrag,
} from "./internalPathDrag";

function transfer(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    effectAllowed: "all" as DataTransfer["effectAllowed"],
    get types() { return [...values.keys()]; },
    getData: (type: string) => values.get(type) ?? "",
    setData: vi.fn((type: string, value: string) => values.set(type, value)),
  };
}

describe("internal path drag payload", () => {
  it("round trips a same-host path and shell-escapes spaces and metacharacters", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/a b'$(touch nope).ts" });
    expect(data.effectAllowed).toBe("copy");
    expect(readInternalPathDrop(data, { hostProfileId: "local", serverIdentity: "server-a" })).toEqual({
      kind: "accepted",
      path: "/repo/a b'$(touch nope).ts",
      shellText: "'/repo/a b'\"'\"'$(touch nope).ts'",
    });
  });

  it("rejects cross-host and malformed internal payloads without producing input", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "remote-a", serverIdentity: "ssh-a", path: "/srv/repo/file" });
    expect(readInternalPathDrop(data, { hostProfileId: "remote-b", serverIdentity: "ssh-b" })).toMatchObject({ kind: "rejected" });
    expect(readInternalPathDrop(transfer({ [INTERNAL_PATH_DRAG_TYPE]: "{}" }), { hostProfileId: "remote-a", serverIdentity: "ssh-a" })).toMatchObject({ kind: "rejected" });
  });

  it("leaves external drops to the existing external-file path", () => {
    expect(readInternalPathDrop(transfer({ "text/uri-list": "file:///tmp/a" }), { hostProfileId: "local", serverIdentity: "local" })).toEqual({ kind: "absent" });
  });

  it("bridges one native empty-path drop on macOS without weakening host validation", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/a b'$(touch nope).ts" });
    expect(claimNativeInternalPathDrag()).toBe(true);
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({
      kind: "accepted",
      path: "/repo/a b'$(touch nope).ts",
      shellText: "'/repo/a b'\"'\"'$(touch nope).ts'",
    });
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "handled" });

    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    claimNativeInternalPathDrag();
    expect(consumeNativeInternalPathDrop({ hostProfileId: "remote", serverIdentity: "server-a" })).toMatchObject({ kind: "rejected" });
  });

  it("clears an unclaimed drag at dragend and keeps a claimed accepted one for the native drop", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    finishInternalPathDrag(true);
    expect(claimNativeInternalPathDrag()).toBe(false);

    // On macOS wry reports the drop through Tauri's async native event, which
    // arrives after WebKit's synchronous dragend. The claimed gesture must
    // still deliver once, then read as handled.
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    expect(claimNativeInternalPathDrag()).toBe(true);
    finishInternalPathDrag(true);
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toMatchObject({
      kind: "accepted",
      path: "/repo/file",
    });
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "handled" });
  });

  it("expires an accepted drag whose native drop never arrives", () => {
    vi.useFakeTimers();
    try {
      const data = transfer();
      writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
      claimNativeInternalPathDrag();
      finishInternalPathDrag(true);
      vi.advanceTimersByTime(2_001);
      expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "absent" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a drag that leaves the window and comes back droppable", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    expect(claimNativeInternalPathDrag()).toBe(true);
    // A native leave is the pointer crossing the window edge, not the end of
    // the DOM gesture: dragging out and back in is one drag, and the re-entry
    // re-claims the same record.
    releaseNativeInternalPathDrag();
    expect(claimNativeInternalPathDrag()).toBe(true);
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toMatchObject({
      kind: "accepted",
      path: "/repo/file",
    });
  });

  it("does not resurrect a cancelled claimed drag into a later pathless drop", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/secret" });
    claimNativeInternalPathDrag();
    // Dropped outside the app, or cancelled with Escape: dragend reports
    // dropEffect "none". The unrelated pathless drop that follows — a text
    // selection, an image — must not paste this path.
    finishInternalPathDrag(false);
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "absent" });
  });

  it("retires the native bridge when the same gesture reaches the DOM lane", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    claimNativeInternalPathDrag();
    expect(readInternalPathDrop(data, { hostProfileId: "local", serverIdentity: "server-a" })).toMatchObject({ kind: "accepted" });
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "handled" });
  });

  it("retires a lost source when its host scope unmounts without clearing another host", () => {
    const data = transfer();
    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    claimNativeInternalPathDrag();
    cancelInternalPathDragSource({ hostProfileId: "other", serverIdentity: "server-a" });
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toMatchObject({ kind: "accepted" });

    writeInternalPathDrag(data, { hostProfileId: "local", serverIdentity: "server-a", path: "/repo/file" });
    claimNativeInternalPathDrag();
    cancelInternalPathDragSource({ hostProfileId: "local", serverIdentity: "server-a" });
    expect(consumeNativeInternalPathDrop({ hostProfileId: "local", serverIdentity: "server-a" })).toEqual({ kind: "absent" });
  });
});
