import { describe, expect, it, vi } from "vitest";
import { INTERNAL_PATH_DRAG_TYPE, readInternalPathDrop, writeInternalPathDrag } from "./internalPathDrag";

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
    writeInternalPathDrag(data, { serverIdentity: "server-a", path: "/repo/a b'$(touch nope).ts" });
    expect(data.effectAllowed).toBe("copy");
    expect(readInternalPathDrop(data, "server-a")).toEqual({
      kind: "accepted",
      path: "/repo/a b'$(touch nope).ts",
      shellText: "'/repo/a b'\"'\"'$(touch nope).ts'",
    });
  });

  it("rejects cross-host and malformed internal payloads without producing input", () => {
    const data = transfer();
    writeInternalPathDrag(data, { serverIdentity: "ssh-a", path: "/srv/repo/file" });
    expect(readInternalPathDrop(data, "ssh-b")).toMatchObject({ kind: "rejected" });
    expect(readInternalPathDrop(transfer({ [INTERNAL_PATH_DRAG_TYPE]: "{}" }), "ssh-a")).toMatchObject({ kind: "rejected" });
  });

  it("leaves external drops to the existing external-file path", () => {
    expect(readInternalPathDrop(transfer({ "text/uri-list": "file:///tmp/a" }), "local")).toEqual({ kind: "absent" });
  });
});
