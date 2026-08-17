import { describe, expect, it, vi } from "vitest";
import { DirectoryWatchLeases, WATCH_RETRY_BASE_MS } from "./watchLeases";
import type { DirectoryListing, DirectoryWatchLease } from "./types";

function listing(rootToken: string, directory: string, overrides: Partial<DirectoryListing> = {}): DirectoryListing {
  return { rootToken, directory, revision: "1", entries: [], recoveredFromOverflow: false, complete: true, ...overrides };
}

function recorder() {
  const acquired: string[] = [];
  const released: string[] = [];
  const bootstrapped: string[] = [];
  const errors: string[] = [];
  let settle: ((directory: string) => void) | undefined;
  const pending = new Map<string, (lease: DirectoryWatchLease) => void>();
  const aborted: string[] = [];
  const host = {
    acquire: (directory: string, signal: AbortSignal) => {
      acquired.push(directory);
      signal.addEventListener("abort", () => aborted.push(directory), { once: true });
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
  return { aborted, acquired, bootstrapped, errors, host, released, settle };
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

  it("stops a bootstrap the tree stopped wanting, and releases one that beat it", async () => {
    const leases = new DirectoryWatchLeases();
    const fixture = recorder();
    leases.sync(["/r"], fixture.host);
    // Collapsed before the bootstrap answered: the listing it is fetching is
    // the expansion's listing, so the remote read is stopped rather than paid
    // for and discarded.
    leases.sync([], fixture.host);
    expect(fixture.aborted).toEqual(["/r"]);
    fixture.settle("/r");
    await Promise.resolve();
    expect(fixture.released).toEqual(["/r"]);
    expect(fixture.bootstrapped).toEqual([]);
    expect(leases.held).toBe(0);
  });

  it("releases rather than aborts a watch that had already arrived", async () => {
    const leases = new DirectoryWatchLeases();
    const fixture = recorder();
    leases.sync(["/r"], fixture.host);
    fixture.settle("/r");
    await Promise.resolve();
    leases.sync([], fixture.host);
    expect(fixture.released).toEqual(["/r"]);
    expect(fixture.aborted, "an armed watch is unwatched, never aborted").toEqual([]);
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

  it("backs a refused watch off instead of retrying it on every toggle", async () => {
    // A refusal used to be retried on every expand or collapse anywhere in the
    // tree, and each retry cost a fallback directory list. Twenty refused
    // directories turned one keystroke into forty remote round trips.
    const leases = new DirectoryWatchLeases();
    const errors: string[] = [];
    const acquire = vi.fn()
      .mockRejectedValueOnce(new Error("watch limit reached"))
      .mockRejectedValueOnce(new Error("watch limit reached"))
      .mockResolvedValue({ snapshot: listing("root", "/r"), release: () => undefined });
    const host = { acquire, onBootstrap: () => undefined, onError: (directory: string) => errors.push(directory) };
    leases.sync(["/r"], host, 0);
    await Promise.resolve();
    await Promise.resolve();
    expect(errors).toEqual(["/r"]);
    expect(leases.held).toBe(0);

    // Every unrelated toggle in the next second asks for nothing.
    for (let toggle = 0; toggle < 20; toggle += 1) leases.sync(["/r"], host, 10 * toggle);
    expect(acquire).toHaveBeenCalledTimes(1);

    // After the wait it tries once more, and backs off further when refused.
    leases.sync(["/r"], host, Date.now() + WATCH_RETRY_BASE_MS + 1);
    await Promise.resolve();
    await Promise.resolve();
    expect(acquire).toHaveBeenCalledTimes(2);
    leases.sync(["/r"], host, Date.now() + WATCH_RETRY_BASE_MS + 1);
    expect(acquire, "the second refusal did not lengthen the wait").toHaveBeenCalledTimes(2);

    // A directory that stops being wanted forgets its refusal outright.
    leases.sync([], host, Date.now());
    leases.sync(["/r"], host, Date.now());
    await Promise.resolve();
    expect(acquire).toHaveBeenCalledTimes(3);
    expect(leases.held).toBe(1);
  });
});
