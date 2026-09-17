import { describe, expect, it } from "vitest";
import {
  EMPTY_LISTINGS,
  NO_RECOVERIES,
  consumeRecoveries,
  installListing,
  onDirectory,
  oweRecovery,
  patchListing,
  pruneSubtree,
  type RecoveryAction,
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

  it("never replaces a pending gap with a page restore", () => {
    // The precedence `oweRecovery` states, from the other side — it used to run
    // backwards here, and not harmlessly: a page restore returns silently on
    // failure or abort without re-queuing anything, so a gap it overwrote was
    // answered by nothing at all.
    const held = listing("/repo", ["a", "b", "c", "d"]);
    const current = state({
      listings: new Map([["/repo", held]]),
      recoveries: new Map([["/repo", { kind: "list", reason: "unmappable" } as const]]),
      loading: new Set(["/repo"]),
    });
    const rescan = listing("/repo", ["a", "b"], { complete: false, nextPageToken: "p2" });
    const next = installListing(current, "/repo", rescan);
    expect(next.listings.get("/repo"), "rows the user can see were deleted").toBe(held);
    expect(
      next.recoveries.get("/repo"),
      "a page restore swallowed a gap that was already owed",
    ).toEqual({ kind: "list", reason: "unmappable" });
    expect(next.loading.has("/repo"), "the directory was left waiting on nothing").toBe(false);
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

describe("consumeRecoveries", () => {
  const restore: RecoveryAction = { kind: "restorePages", entries: 4 };
  const gap: RecoveryAction = { kind: "list", reason: "missing" };
  const queue = (...entries: [string, RecoveryAction][]) => new Map<string, RecoveryAction>(entries);

  it("clears the queue it was handed when nothing arrived meanwhile", () => {
    const issued = queue(["/repo", restore]);
    expect(consumeRecoveries(state({ recoveries: issued }), issued).recoveries).toBe(NO_RECOVERIES);
  });

  /**
   * The exact failure: an unrelated arrival used to make the whole map look
   * undrained, so the caller's effect re-ran and issued `/repo`'s restore a
   * second time. The second restore aborts the first and the aborted one's
   * teardown clears `/repo`'s wait state while its replacement is still
   * running — up to eight sequential round trips, restarted, during the
   * overflow burst that is the only thing producing restores.
   */
  it("keeps an entry raised meanwhile without re-owing the ones it just issued", () => {
    const issued = queue(["/repo", restore]);
    const arrived = queue(["/repo", restore], ["/repo/sub", gap]);
    const next = consumeRecoveries(state({ recoveries: arrived }), issued);
    expect([...next.recoveries]).toEqual([["/repo/sub", gap]]);
  });

  /**
   * `oweRecovery`'s precedence, from the drain's side. A gap raised while the
   * restore was in flight replaced the action object, so it does not match and
   * stays queued — the restore it supersedes returns silently on failure or
   * abort without re-queuing anything, so consuming it here would answer that
   * gap with nothing at all.
   */
  it("leaves a directory whose action was superseded while its read was in flight", () => {
    const issued = queue(["/repo", restore]);
    const superseded = queue(["/repo", gap]);
    const next = consumeRecoveries(state({ recoveries: superseded }), issued);
    expect(next.recoveries.get("/repo")).toBe(gap);
  });

  it("returns the same state when it has nothing of its own to drop", () => {
    const current = state({ recoveries: queue(["/repo/sub", gap]) });
    expect(consumeRecoveries(current, queue(["/repo", restore]))).toBe(current);
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
