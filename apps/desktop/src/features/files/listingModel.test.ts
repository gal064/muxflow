import { describe, expect, it } from "vitest";
import {
  appendPage,
  compareEntries,
  isPatchable,
  isRecoveryReason,
  parentPath,
  patchEntry,
  reachableWatchTargets,
  removeEntry,
} from "./listingModel";
import type { DirectoryListing, FileEntry } from "./types";

function entry(path: string, directory = false): FileEntry {
  return {
    path,
    name: path.split("/").at(-1) ?? path,
    kind: directory ? "directory" : "file",
    sizeBytes: "1",
    modifiedMillis: "1",
    generation: "1",
    executable: false,
    expandable: directory,
  };
}

function listing(entries: FileEntry[], overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return {
    rootToken: "root", directory: "/r", revision: "1", entries,
    recoveredFromOverflow: false, complete: true, ...overrides,
  };
}

describe("listingModel", () => {
  it("inserts a new entry in the host's own order rather than at the end", () => {
    const patched = patchEntry(listing([entry("/r/src", true), entry("/r/b.txt")]), entry("/r/a.txt"));
    expect(isRecoveryReason(patched)).toBe(false);
    if (isRecoveryReason(patched)) return;
    expect(patched.entries.map((item) => item.name)).toEqual(["src", "a.txt", "b.txt"]);
  });

  it("replaces an entry in place without moving it", () => {
    const updated = { ...entry("/r/b.txt"), sizeBytes: "99", generation: "7" };
    const patched = patchEntry(listing([entry("/r/a.txt"), entry("/r/b.txt")]), updated);
    if (isRecoveryReason(patched)) throw new Error("expected a patch");
    expect(patched.entries.map((item) => item.name)).toEqual(["a.txt", "b.txt"]);
    expect(patched.entries[1]).toMatchObject({ sizeBytes: "99", generation: "7" });
  });

  it("refuses to patch anything but a complete listing", () => {
    expect(patchEntry(undefined, entry("/r/a.txt"))).toBe("missing");
    expect(removeEntry(undefined, "/r/a.txt")).toBe("missing");
    const partial = listing([entry("/r/a.txt")], { complete: false, nextPageToken: "opaque" });
    expect(isPatchable(partial)).toBe(false);
    // A page boundary hides entries on both sides of it: an insert would be
    // drawn as though the tree had reached it, and an absence proves nothing.
    expect(patchEntry(partial, entry("/r/z.txt"))).toBe("incomplete");
    expect(removeEntry(partial, "/r/a.txt")).toBe("incomplete");
  });

  it("removes a deleted entry and treats an absent one as already applied", () => {
    const source = listing([entry("/r/a.txt"), entry("/r/b.txt")]);
    const removed = removeEntry(source, "/r/a.txt");
    if (isRecoveryReason(removed)) throw new Error("expected a removal");
    expect(removed.entries.map((item) => item.name)).toEqual(["b.txt"]);
    expect(removeEntry(source, "/r/never")).toBe(source);
  });

  it("orders directories before files and then by name", () => {
    const sorted = [entry("/r/z.txt"), entry("/r/a.txt"), entry("/r/lib", true)].sort(compareEntries);
    expect(sorted.map((item) => item.name)).toEqual(["lib", "a.txt", "z.txt"]);
  });

  it("appends a later page without losing the rows already shown", () => {
    const first = listing([entry("/r/a.txt")], { complete: false, nextPageToken: "one" });
    const second = listing([entry("/r/b.txt")], { complete: true, recoveredFromOverflow: true });
    const joined = appendPage(first, second);
    expect(joined.entries.map((item) => item.name)).toEqual(["a.txt", "b.txt"]);
    expect(joined.complete).toBe(true);
    expect(joined.recoveredFromOverflow).toBe(true);
  });

  it("owes a watch only to directories the tree can currently reach", () => {
    const listings = new Map<string, DirectoryListing>([
      ["/r", listing([entry("/r/src", true), entry("/r/docs", true)], { directory: "/r" })],
      ["/r/src", listing([entry("/r/src/deep", true)], { directory: "/r/src" })],
      ["/r/src/deep", listing([], { directory: "/r/src/deep" })],
    ]);
    // Everything open: the root and both open descendants are reachable.
    expect(reachableWatchTargets("/r", listings, new Set(["/r", "/r/src", "/r/src/deep"])))
      .toEqual(["/r", "/r/src", "/r/src/deep"]);
    // Collapsing the parent hides the descendant, so its watch is not owed —
    // even though the tree still remembers that the descendant was open.
    expect(reachableWatchTargets("/r", listings, new Set(["/r", "/r/src/deep"]))).toEqual(["/r"]);
    // A directory nobody expanded is never watched.
    expect(reachableWatchTargets("/r", listings, new Set(["/r"]))).toEqual(["/r"]);
  });

  it("reads a parent path without inventing one above the filesystem root", () => {
    expect(parentPath("/r/a.txt")).toBe("/r");
    expect(parentPath("/a")).toBe("/");
    expect(parentPath("/")).toBe("/");
  });
});
