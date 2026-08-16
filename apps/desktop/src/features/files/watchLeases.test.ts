import { describe, expect, it, vi } from "vitest";
import { DirectoryListingCache } from "./directoryCache";
import { DirectoryWatchLeases } from "./watchLeases";
import type { DirectoryListing, DirectoryWatchLease } from "./types";

function listing(rootToken: string, directory: string, overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return { rootToken, directory, revision: "1", entries: [], overflowRecovery: false, complete: true, ...overrides };
}

function recorder() {
  const acquired: string[] = [];
  const released: string[] = [];
  const bootstrapped: string[] = [];
  const errors: string[] = [];
  let settle: ((directory: string) => void) | undefined;
  const pending = new Map<string, (lease: DirectoryWatchLease) => void>();
  const host = {
    acquire: (directory: string) => {
      acquired.push(directory);
      return new Promise<DirectoryWatchLease>((resolve) => {
        pending.set(directory, resolve);
      });
    },
    onBootstrap: (directory: string) => { bootstrapped.push(directory); },
    onError: (directory: string) => { errors.push(directory); },
  };
  settle = (directory: string) => {
    const resolve = pending.get(directory);
    pending.delete(directory);
    resolve?.({ snapshot: listing("root", directory), release: () => released.push(directory) });
  };
  return { acquired, bootstrapped, errors, host, released, settle };
}

describe("DirectoryWatchLeases", () => {
  it("acquires and releases only the directories that changed", async () => {
    const leases = new DirectoryWatchLeases();
    const fixture = recorder();
    leases.sync(["/r"], fixture.host);
    fixture.settle("/r");
    await Promise.resolve();
    expect(fixture.acquired).toEqual(["/r"]);

    // Opening one folder must not touch the watch the root already holds.
    leases.sync(["/r", "/r/src"], fixture.host);
    fixture.settle("/r/src");
    await Promise.resolve();
    expect(fixture.acquired).toEqual(["/r", "/r/src"]);
    expect(fixture.released).toEqual([]);
    expect(fixture.bootstrapped).toEqual(["/r", "/r/src"]);

    // Closing it is exactly one release.
    leases.sync(["/r"], fixture.host);
    expect(fixture.released).toEqual(["/r/src"]);
    expect(leases.held).toBe(1);
  });

  it("releases a watch that arrives after the directory stopped being wanted", async () => {
    const leases = new DirectoryWatchLeases();
    const fixture = recorder();
    leases.sync(["/r"], fixture.host);
    leases.sync([], fixture.host);
    fixture.settle("/r");
    await Promise.resolve();
    expect(fixture.released).toEqual(["/r"]);
    expect(fixture.bootstrapped).toEqual([]);
    expect(leases.held).toBe(0);
  });

  it("releases everything exactly once on teardown", async () => {
    const leases = new DirectoryWatchLeases();
    const fixture = recorder();
    leases.sync(["/r", "/r/src"], fixture.host);
    fixture.settle("/r");
    fixture.settle("/r/src");
    await Promise.resolve();
    leases.releaseAll();
    leases.releaseAll();
    expect(fixture.released).toEqual(["/r", "/r/src"]);
  });

  it("reports a failed acquisition and forgets it, so a later sync can retry", async () => {
    const leases = new DirectoryWatchLeases();
    const errors: string[] = [];
    const acquire = vi.fn()
      .mockRejectedValueOnce(new Error("watch limit reached"))
      .mockResolvedValueOnce({ snapshot: listing("root", "/r"), release: () => undefined });
    const host = { acquire, onBootstrap: () => undefined, onError: (directory: string) => errors.push(directory) };
    leases.sync(["/r"], host);
    await Promise.resolve();
    await Promise.resolve();
    expect(errors).toEqual(["/r"]);
    expect(leases.held).toBe(0);
    leases.sync(["/r"], host);
    await Promise.resolve();
    expect(acquire).toHaveBeenCalledTimes(2);
  });
});

describe("DirectoryListingCache", () => {
  it("retains only complete listings bound to the exact connection and root", () => {
    const cache = new DirectoryListingCache();
    const key = { clientId: "c", rootToken: "root", directory: "/r" };
    cache.set(key, listing("root", "/r", { complete: false, nextPageToken: "opaque" }));
    expect(cache.get(key), "a partial page would paint a directory smaller than it is").toBeUndefined();
    cache.set(key, listing("other", "/r"));
    expect(cache.get(key), "a listing from another root capability is a different file tree").toBeUndefined();
    cache.set(key, listing("root", "/r"));
    expect(cache.get(key)).toBeDefined();
    expect(cache.get({ ...key, clientId: "reconnected" })).toBeUndefined();
  });

  it("drops everything a replaced root no longer authorises", () => {
    const cache = new DirectoryListingCache();
    cache.set({ clientId: "c", rootToken: "root", directory: "/r" }, listing("root", "/r"));
    cache.set({ clientId: "c", rootToken: "root", directory: "/r/src" }, listing("root", "/r/src"));
    cache.set({ clientId: "c", rootToken: "next", directory: "/other" }, listing("next", "/other"));
    cache.invalidateOtherRoots("c", "next");
    expect(cache.size).toBe(1);
    expect(cache.get({ clientId: "c", rootToken: "next", directory: "/other" })).toBeDefined();
  });

  it("drops a deleted directory and everything cached beneath it", () => {
    const cache = new DirectoryListingCache();
    for (const directory of ["/r", "/r/src", "/r/src/deep", "/r/srcfile"]) {
      cache.set({ clientId: "c", rootToken: "root", directory }, listing("root", directory));
    }
    cache.invalidateSubtree("c", "root", "/r/src");
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r/src" })).toBeUndefined();
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r/src/deep" })).toBeUndefined();
    // A sibling that merely shares a prefix is a different directory.
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r/srcfile" })).toBeDefined();
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r" })).toBeDefined();
  });

  it("bounds itself by evicting the least recently used directory", () => {
    const cache = new DirectoryListingCache();
    for (let index = 0; index < 300; index += 1) {
      cache.set({ clientId: "c", rootToken: "root", directory: `/r/${index}` }, listing("root", `/r/${index}`));
    }
    expect(cache.size).toBe(256);
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r/0" })).toBeUndefined();
    expect(cache.get({ clientId: "c", rootToken: "root", directory: "/r/299" })).toBeDefined();
  });
});
