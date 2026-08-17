import { describe, expect, it } from "vitest";
import { DirectoryListingCache } from "./directoryCache";
import type { DirectoryListing } from "./types";

function listing(rootToken: string, directory: string, overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return { rootToken, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true, ...overrides };
}

describe("DirectoryListingCache", () => {
  it("retains only complete listings bound to the exact connection and root", () => {
    const cache = new DirectoryListingCache();
    const key = { clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r" };
    cache.set(key, listing("root", "/r", { complete: false, nextPageToken: "opaque" }));
    expect(cache.get(key), "a partial page would paint a directory smaller than it is").toBeUndefined();
    cache.set(key, listing("other", "/r"));
    expect(cache.get(key), "a listing from another root capability is a different file tree").toBeUndefined();
    cache.set(key, listing("root", "/r"));
    expect(cache.get(key)).toBeDefined();
    expect(cache.get({ ...key, clientId: "reconnected" })).toBeUndefined();
    // A root re-resolved under a fresh generation is a fresh answer, even when
    // the capability token is unchanged.
    expect(cache.get({ ...key, rootGeneration: "2" })).toBeUndefined();
  });

  it("drops everything a replaced root no longer authorises", () => {
    const cache = new DirectoryListingCache();
    cache.set({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r" }, listing("root", "/r"));
    cache.set({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/src" }, listing("root", "/r/src"));
    cache.set({ clientId: "c", rootToken: "next", rootGeneration: "2", directory: "/other" }, listing("next", "/other"));
    cache.invalidateOtherRoots("c", "next", "2");
    expect(cache.size).toBe(1);
    expect(cache.get({ clientId: "c", rootToken: "next", rootGeneration: "2", directory: "/other" })).toBeDefined();
  });

  it("drops a deleted directory and everything cached beneath it", () => {
    const cache = new DirectoryListingCache();
    for (const directory of ["/r", "/r/src", "/r/src/deep", "/r/srcfile"]) {
      cache.set({ clientId: "c", rootToken: "root", rootGeneration: "1", directory }, listing("root", directory));
    }
    cache.invalidateSubtree({ clientId: "c", rootToken: "root", rootGeneration: "1" }, "/r/src");
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/src" })).toBeUndefined();
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/src/deep" })).toBeUndefined();
    // A sibling that merely shares a prefix is a different directory.
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/srcfile" })).toBeDefined();
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r" })).toBeDefined();
  });

  it("bounds itself by evicting the least recently used directory", () => {
    const cache = new DirectoryListingCache();
    for (let index = 0; index < 300; index += 1) {
      cache.set({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: `/r/${index}` }, listing("root", `/r/${index}`));
    }
    expect(cache.size).toBe(256);
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/0" })).toBeUndefined();
    expect(cache.get({ clientId: "c", rootToken: "root", rootGeneration: "1", directory: "/r/299" })).toBeDefined();
  });
});
