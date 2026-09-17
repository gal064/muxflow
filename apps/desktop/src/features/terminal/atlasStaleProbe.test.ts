import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  __resetAtlasProbeForTests,
  __sampleAtlasProbeForTests,
  readAtlasInvalidationCount,
  watchAtlasStaleness,
} from "./atlasStaleProbe";

const recorded: { kind: string; detail?: Record<string, unknown> }[] = [];

vi.mock("../../diagnostics/incidents", () => ({
  recordIncident: (kind: string, detail?: Record<string, unknown>) => {
    recorded.push({ kind, detail });
  },
}));

/**
 * Stands in for a `WebglAddon`, down to the private path the probe walks. The
 * probe never touches WebGL — it compares two numbers the addon keeps — so a
 * literal of the same shape exercises every branch it has.
 */
function addon(atlasCount: number, rendererVersion: number): {
  _renderer: { _charAtlas: { _requestClearModel: number }; _glyphRenderer: { value: { _atlasClearModelVersion: number } } };
} {
  return {
    _renderer: {
      _charAtlas: { _requestClearModel: atlasCount },
      _glyphRenderer: { value: { _atlasClearModelVersion: rendererVersion } },
    },
  };
}

let now = 0;

beforeEach(() => {
  recorded.length = 0;
  now = 0;
  __resetAtlasProbeForTests({ now: () => now });
});

afterEach(() => {
  __resetAtlasProbeForTests();
});

describe("a pane left behind by the shared atlas", () => {
  it("is reported once it has stayed behind across two samples", () => {
    const pane = addon(3, 2);
    watchAtlasStaleness("pane-1", pane);

    // One sample proves nothing: the repaint that answers the merge may not
    // have run yet. This is the healthy case, and it must stay silent.
    __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasStale")).toHaveLength(0);

    __sampleAtlasProbeForTests();
    const stale = recorded.filter((r) => r.kind === "render.atlasStale");
    expect(stale).toHaveLength(1);
    expect(stale[0].detail).toMatchObject({ paneId: "pane-1", atlasCount: 3, rendererVersion: 2 });
  });

  it("is reported once per episode, not once per sample", () => {
    watchAtlasStaleness("pane-1", addon(1, 0));
    for (let i = 0; i < 6; i++) __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasStale")).toHaveLength(1);
  });

  it("is not reported at all once a frame catches it up", () => {
    const pane = addon(1, 0);
    __sampleAtlasProbeForTests();
    watchAtlasStaleness("pane-1", pane);
    __sampleAtlasProbeForTests();
    // The repaint lands between the two samples, which is what the fix makes
    // happen — so the pane is never two samples behind.
    pane._renderer._glyphRenderer.value._atlasClearModelVersion = 1;
    __sampleAtlasProbeForTests();
    __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasStale")).toHaveLength(0);
  });

  it("is not claimed of a pane that has never drawn a frame", () => {
    // -1 is what a `GlyphRenderer` carries until its first `beginFrame`, which
    // is the state of a pane that is hidden or has not been revealed yet. It
    // has nothing baked to be wrong about.
    watchAtlasStaleness("hidden-pane", addon(2, -1));
    __sampleAtlasProbeForTests();
    __sampleAtlasProbeForTests();
    __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasStale")).toHaveLength(0);
  });

  it("says nothing when the addon no longer has the shape it reads", () => {
    watchAtlasStaleness("pane-1", { _renderer: { _charAtlas: {}, _glyphRenderer: {} } });
    watchAtlasStaleness("pane-2", {});
    watchAtlasStaleness("pane-3", undefined);
    __sampleAtlasProbeForTests();
    __sampleAtlasProbeForTests();
    expect(recorded).toHaveLength(0);
  });
});

describe("the atlas invalidation rate line", () => {
  it("is written when the count moves, and at most once a minute", () => {
    const pane = addon(0, 0);
    watchAtlasStaleness("pane-1", pane);
    __sampleAtlasProbeForTests();

    pane._renderer._charAtlas._requestClearModel = 2;
    pane._renderer._glyphRenderer.value._atlasClearModelVersion = 2;
    now = 60_001;
    __sampleAtlasProbeForTests();
    const first = recorded.filter((r) => r.kind === "render.atlasInvalidations");
    expect(first).toHaveLength(1);
    expect(first[0].detail).toMatchObject({ count: 2, delta: 2 });

    // A second burst inside the same minute is not worth a second line.
    pane._renderer._charAtlas._requestClearModel = 5;
    pane._renderer._glyphRenderer.value._atlasClearModelVersion = 5;
    __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasInvalidations")).toHaveLength(1);
  });

  it("stays silent while the count holds still", () => {
    watchAtlasStaleness("pane-1", addon(4, 4));
    now = 10 * 60_000;
    __sampleAtlasProbeForTests();
    __sampleAtlasProbeForTests();
    expect(recorded.filter((r) => r.kind === "render.atlasInvalidations")).toHaveLength(0);
  });
});

describe("the invalidation count read on demand", () => {
  it("reads the registered pane's current count, and moves with it", () => {
    const pane = addon(3, 3);
    const stop = watchAtlasStaleness("pane-1", pane);
    expect(readAtlasInvalidationCount("pane-1")).toBe(3);

    pane._renderer._charAtlas._requestClearModel = 4;
    expect(readAtlasInvalidationCount("pane-1")).toBe(4);

    // A pane that has gone away has no count, and neither does one that never
    // registered or whose addon no longer has the field.
    stop();
    expect(readAtlasInvalidationCount("pane-1")).toBeUndefined();
    expect(readAtlasInvalidationCount("never-mounted")).toBeUndefined();
    watchAtlasStaleness("pane-2", { _renderer: { _charAtlas: {} } });
    expect(readAtlasInvalidationCount("pane-2")).toBeUndefined();
    watchAtlasStaleness("pane-3", undefined);
    expect(readAtlasInvalidationCount("pane-3")).toBeUndefined();
  });
});

describe("the sampler's timer", () => {
  it("arms with the first pane and disarms with the last", () => {
    const started: number[] = [];
    let cleared = 0;
    __resetAtlasProbeForTests({
      setInterval: (_handler, ms) => {
        started.push(ms);
        return "handle";
      },
      clearInterval: () => {
        cleared++;
      },
      now: () => now,
    });

    const stopA = watchAtlasStaleness("a", addon(0, 0));
    const stopB = watchAtlasStaleness("b", addon(0, 0));
    expect(started).toEqual([5_000]);

    stopA();
    expect(cleared).toBe(0);
    stopB();
    expect(cleared).toBe(1);

    // And arms again for a pane mounted after the last one went away.
    watchAtlasStaleness("c", addon(0, 0));
    expect(started).toEqual([5_000, 5_000]);
  });
});
