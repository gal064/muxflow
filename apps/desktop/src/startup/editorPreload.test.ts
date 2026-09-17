// @vitest-environment jsdom
// jsdom, for a `globalThis` that a missing `requestIdleCallback` can be
// installed on and removed from per case.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Whether the editor bundle has been evaluated, and how often.
 *
 * The mirror image of `editorChunkAbsence.test.tsx`'s probe, and mocked the
 * same way: the claim worth pinning is not "some promise resolved" but "the
 * editor module specifically was the thing imported". A test that watched a
 * spy would keep passing if the import were pointed at the wrong module, which
 * is the mistake most worth catching here.
 *
 * The real module is never evaluated — Monaco registers workers and reads the
 * live token file on import, neither of which this file is about.
 */
const probe = vi.hoisted(() => ({ imported: 0 }));

/**
 * Re-registers the editor mock and hands back a freshly evaluated module.
 *
 * `vi.mock` is hoisted and its factory is memoized per registration, so it runs
 * exactly once for the whole file no matter how many times the registry is
 * reset — which would make `probe.imported` mean "some earlier case imported
 * the editor" and quietly reduce every case after the first to a tautology.
 * `vi.doMock` is not hoisted, so re-registering it per case and then resetting
 * the registry is what actually re-evaluates the module and makes the count
 * mean "*this* case imported the editor".
 */
async function freshPreload(): Promise<typeof import("./editorPreload")> {
  vi.doMock("../features/files/FileEditor", () => {
    probe.imported += 1;
    return { FileEditor: () => null };
  });
  vi.resetModules();
  return await import("./editorPreload");
}

/**
 * The two globals this file installs and withdraws.
 *
 * Deliberately not an intersection with `typeof globalThis`: the DOM lib
 * declares both as required, and an intersection keeps that requiredness, so
 * the absent-`requestIdleCallback` cases could not express the very state they
 * exist to test.
 */
type IdleGlobals = {
  requestIdleCallback?: (callback: IdleRequestCallback, options?: IdleRequestOptions) => number;
  cancelIdleCallback?: (handle: number) => void;
};

const idleGlobals = globalThis as unknown as IdleGlobals;
const originalRequest = idleGlobals.requestIdleCallback;
const originalCancel = idleGlobals.cancelIdleCallback;

/** The idle callbacks scheduled so far, with the options they asked for. */
let scheduled: { callback: IdleRequestCallback; options?: IdleRequestOptions; cancelled: boolean }[];

/** The module under test, re-evaluated per case. See `freshPreload`. */
let preload: typeof import("./editorPreload");

beforeEach(async () => {
  probe.imported = 0;
  scheduled = [];
  idleGlobals.requestIdleCallback = (callback, options) =>
    scheduled.push({ callback, options, cancelled: false });
  idleGlobals.cancelIdleCallback = (handle) => {
    const entry = scheduled[handle - 1];
    if (entry) entry.cancelled = true;
  };
  preload = await freshPreload();
});

afterEach(() => {
  idleGlobals.requestIdleCallback = originalRequest;
  idleGlobals.cancelIdleCallback = originalCancel;
  vi.restoreAllMocks();
});

/** Runs the pending idle callbacks that were not withdrawn. */
function goIdle() {
  for (const entry of scheduled) {
    if (!entry.cancelled) {
      entry.callback({ didTimeout: false, timeRemaining: () => 0 });
    }
  }
}

/**
 * Waits until the editor has been imported.
 *
 * Polled rather than counted in microtask turns: a dynamic import resolves
 * through the module loader, not on a fixed number of turns, so "await twice
 * and assert" is a race that happens to pass or fail depending on how much the
 * loader had cached. This waits for the outcome instead of guessing its timing.
 */
async function importedOnce() {
  await vi.waitFor(() => {
    expect(probe.imported).toBe(1);
  });
}

/**
 * Gives a preload that *was* going to happen every chance to happen.
 *
 * The negative cases need this: asserting "still zero" immediately would pass
 * even if the withdrawal did nothing at all. A macrotask plus a drained
 * microtask queue is more than the positive cases above ever need.
 */
async function settle() {
  await new Promise((resolve) => { setTimeout(resolve, 0); });
  await Promise.resolve();
  await Promise.resolve();
}

describe("the editor preload", () => {
  it("does not fetch the editor merely by being scheduled", async () => {
    preload.scheduleEditorPreload();
    await Promise.resolve();
    expect(
      probe.imported,
      "the preload fetched the editor eagerly, which is the cost it exists to move",
    ).toBe(0);
    expect(scheduled).toHaveLength(1);
  });

  it("fetches the editor once the page goes idle", async () => {
    preload.scheduleEditorPreload();
    goIdle();
    await importedOnce();
  });

  it("bounds the wait, so a never-idle shell still preloads", () => {
    preload.scheduleEditorPreload();
    expect(scheduled[0]?.options?.timeout).toBe(preload.EDITOR_PRELOAD_TIMEOUT_MS);
  });

  it("does not fetch the editor after the schedule is cancelled", async () => {
    const cancel = preload.scheduleEditorPreload();
    cancel();
    goIdle();
    await settle();
    expect(probe.imported).toBe(0);
  });

  it("falls back to a timer where idle callbacks do not exist", async () => {
    // Assigned, not deleted: jsdom carries its own `requestIdleCallback`, so
    // deleting the stub only uncovers the real one and the fallback under test
    // never runs.
    idleGlobals.requestIdleCallback = undefined;
    idleGlobals.cancelIdleCallback = undefined;
    preload.scheduleEditorPreload();
    expect(probe.imported, "the fallback fired synchronously instead of yielding").toBe(0);
    await importedOnce();
  });

  it("withdraws the fallback timer too", async () => {
    // Assigned, not deleted: jsdom carries its own `requestIdleCallback`, so
    // deleting the stub only uncovers the real one and the fallback under test
    // never runs.
    idleGlobals.requestIdleCallback = undefined;
    idleGlobals.cancelIdleCallback = undefined;
    preload.scheduleEditorPreload()();
    await settle();
    expect(probe.imported).toBe(0);
  });

  it("swallows a failed fetch rather than rejecting out of an idle callback", async () => {
    // Re-mocked for this case rather than driven by the shared factory's
    // `fail` flag: the hoisted factory is memoized per mock registration, so
    // once any earlier case has imported the editor successfully the flag can
    // never be observed again. `doMock` is not hoisted, so paired with a
    // registry reset it is what actually re-evaluates the module.
    vi.doMock("../features/files/FileEditor", () => {
      throw new Error("chunk unavailable");
    });
    vi.resetModules();
    try {
      const failing = await import("./editorPreload");
      const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
      await expect(
        failing.preloadEditorChunk(),
        "an unhandled rejection for a fault the lazy boundary already handles",
      ).resolves.toBeUndefined();
      expect(warn).toHaveBeenCalled();
    } finally {
      vi.doUnmock("../features/files/FileEditor");
    }
  });
});
