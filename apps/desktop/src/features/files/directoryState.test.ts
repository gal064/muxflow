import { describe, expect, it } from "vitest";
import {
  EMPTY_LISTINGS,
  NO_RECOVERIES,
  installListing,
  onDirectory,
  oweRecovery,
  patchListing,
  pruneSubtree,
  type WorkspaceFilesState,
} from "./directoryState";
import type { ActiveRoot, DirectoryListing, FileEntry } from "./types";

const root: ActiveRoot = { token: "root", paneId: "%1", cwd: "/repo", path: "/repo", gitWorktree: true, revision: "1" };
const other: ActiveRoot = { ...root, token: "other", path: "/elsewhere" };

function entry(path: string): FileEntry {
  return {
    path, name: path.split("/").at(-1) ?? path, kind: "file", sizeBytes: "1",
    modifiedMillis: "1", generation: "1", executable: false, expandable: false,
  };
}

function listing(directory: string, names: string[], overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return {
    rootToken: "root", directory, revision: "5",
    entries: names.map((name) => entry(`${directory}/${name}`)),
    recoveredFromOverflow: false, complete: true, ...overrides,
  };
}

function state(overrides: Partial<WorkspaceFilesState> = {}): WorkspaceFilesState {
  return {
    scopeKey: "scope", root, listings: EMPTY_LISTINGS, expanded: new Set(["/repo"]),
    loading: new Set(), requestedReads: 0, recoveries: NO_RECOVERIES, ...overrides,
  };
}

describe("onDirectory", () => {
  it("refuses a transition raised under a root that has since been replaced", () => {
    const current = state();
    expect(onDirectory(current, other, "/repo", () => state({ requestedReads: 9 }))).toBe(current);
  });

  it("refuses a transition for a directory the tree is no longer showing", () => {
    // A snapshot that races a collapse, or a recovery list whose directory was
    // deleted underneath it, must not put rows back into a tree that cannot
    // reach them — including the root's own parent, for an event about the root.
    const current = state({ expanded: new Set() });
    expect(onDirectory(current, root, "/repo", () => state({ requestedReads: 9 }))).toBe(current);
  });

  it("runs a transition for a directory the tree is showing under the root that authorised it", () => {
    const current = state();
    expect(onDirectory(current, root, "/repo", (held) => ({ ...held, requestedReads: 9 })).requestedReads).toBe(9);
  });
});

describe("installListing", () => {
  it("clears the directory's wait state however the listing was decided", () => {
    for (const incoming of [listing("/repo", ["a"]), listing("/repo", [], { revision: "1" })]) {
      const current = state({
        listings: new Map([["/repo", listing("/repo", ["a", "b"])]]),
        loading: new Set(["/repo"]),
      });
      expect(installListing(current, "/repo", incoming).loading.has("/repo")).toBe(false);
    }
  });

  it("keeps the newer of two listings whichever order they arrived in", () => {
    // Three producers write this slot and all of them mint a revision before a
    // blocking scan, so arrival order is not freshness order.
    const held = listing("/repo", ["new"], { revision: "9" });
    const current = state({ listings: new Map([["/repo", held]]) });
    const late = listing("/repo", ["old"], { revision: "4" });
    expect(installListing(current, "/repo", late).listings.get("/repo")).toBe(held);
  });

  it("compares revisions it cannot order by not comparing them at all", () => {
    const held = listing("/repo", ["held"], { revision: "not-a-number" });
    const current = state({ listings: new Map([["/repo", held]]) });
    const incoming = listing("/repo", ["incoming"], { revision: "1" });
    expect(installListing(current, "/repo", incoming).listings.get("/repo")).toBe(incoming);
  });

  it("never shortens a paginated directory, and says what it owes instead", () => {
    const held = listing("/repo", ["a", "b", "c", "d"]);
    const current = state({ listings: new Map([["/repo", held]]) });
    const rescan = listing("/repo", ["a", "b"], { complete: false, nextPageToken: "p2" });
    const next = installListing(current, "/repo", rescan);
    expect(next.listings.get("/repo"), "rows the user can see were deleted").toBe(held);
    expect(next.recoveries.get("/repo")).toEqual({ kind: "restorePages", entries: 4 });
  });

  it("refuses a page that continues a listing the tree no longer holds", () => {
    const held = listing("/repo", ["a"], { revision: "9" });
    const current = state({ listings: new Map([["/repo", held]]) });
    const stale = listing("/repo", ["gone"], { revision: "5" });
    expect(installListing(current, "/repo", stale, { append: true }).listings.get("/repo")).toBe(held);
  });

  it("appends a page of the listing it is holding", () => {
    const held = listing("/repo", ["a"], { complete: false, nextPageToken: "p2" });
    const current = state({ listings: new Map([["/repo", held]]) });
    const page = listing("/repo", ["b"]);
    const merged = installListing(current, "/repo", page, { append: true }).listings.get("/repo");
    expect(merged?.entries.map((row) => row.name)).toEqual(["a", "b"]);
    expect(merged?.complete).toBe(true);
  });
});

describe("oweRecovery", () => {
  it("lets a genuine gap supersede a pending page restore, never the reverse", () => {
    const paged = state({ recoveries: new Map([["/repo", { kind: "restorePages", entries: 4 } as const]]) });
    expect(oweRecovery(paged, "/repo", "missing").recoveries.get("/repo")).toEqual({ kind: "list", reason: "missing" });
    const listed = state({ recoveries: new Map([["/repo", { kind: "list", reason: "missing" } as const]]) });
    expect(oweRecovery(listed, "/repo", "unmappable")).toBe(listed);
  });
});

describe("pruneSubtree", () => {
  it("takes a deleted directory's whole subtree with it, and nothing beside it", () => {
    const current = state({
      listings: new Map([
        ["/repo", listing("/repo", ["src"])],
        ["/repo/src", listing("/repo/src", ["a"])],
        ["/repo/src/deep", listing("/repo/src/deep", [])],
        ["/repo/srcery", listing("/repo/srcery", [])],
      ]),
      expanded: new Set(["/repo", "/repo/src", "/repo/src/deep", "/repo/srcery"]),
      loading: new Set(["/repo/src/deep"]),
    });
    const next = pruneSubtree(current, "/repo/src");
    expect([...next.listings.keys()]).toEqual(["/repo", "/repo/srcery"]);
    expect([...next.expanded]).toEqual(["/repo", "/repo/srcery"]);
    expect(next.loading.size).toBe(0);
  });

  it("is the same state when nothing beneath the path was held", () => {
    const current = state();
    expect(pruneSubtree(current, "/repo/absent")).toBe(current);
  });
});

describe("patchListing", () => {
  it("replaces one directory and leaves every other map entry identical", () => {
    const sibling = listing("/repo/other", ["x"]);
    const current = state({
      listings: new Map([["/repo", listing("/repo", ["a"])], ["/repo/other", sibling]]),
    });
    const next = patchListing(current, "/repo", listing("/repo", ["a", "b"]));
    expect(next.listings.get("/repo")?.entries).toHaveLength(2);
    expect(next.listings.get("/repo/other")).toBe(sibling);
  });
});
