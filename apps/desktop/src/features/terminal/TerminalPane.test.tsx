// @vitest-environment jsdom
// The perceived-latency spans (create.tab, window.switch, …) end at a pane's
// first real paint, and every regression here shipped because only the probe's
// bookkeeping was tested, never the pane lifecycle that has to call it. These
// tests mount the real TerminalPane against a scripted hub and renderer and
// assert the span outcome the user's interaction would produce.
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Pane } from "../../app/types";
import {
  abandonPanePaintSpansForScope,
  enablePerfProbe,
  openPanePaintSpan,
  perfCounterSnapshot,
  perfSummary,
  resetPerfProbe,
  targetPanePaintSpan,
} from "../../perf/probe";

const api = vi.hoisted(() => ({
  setTerminalVisibility: vi.fn(async (..._args: unknown[]) => undefined),
  requestTerminalSeed: vi.fn(async (..._args: unknown[]) => undefined),
}));
vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  setTerminalVisibility: api.setTerminalVisibility,
  requestTerminalSeed: api.requestTerminalSeed,
}));

const { FakeRenderer, renderers } = vi.hoisted(() => {
  type Size = { columns: number; rows: number };
  // What `measure()` answers from the moment the pane opens the renderer, which
  // is before a test can reach the instance.
  const config: { measured?: Size; wedgeDrain?: boolean } = {};
  class FakeRenderer {
    writes: string[] = [];
    disposed = false;
    focusCalls = 0;
    restoredSerialized: string | undefined;
    restores: string[] = [];
    measured = config.measured;
    /** Mirrors the real renderer: `setGrid` is the only writer of cols/rows. */
    grid: Size = { columns: 80, rows: 24 };
    resizes: Size[] = [];
    #pendingRendered: Array<() => void> = [];

    open(): void {}
    measure(): Size | undefined { return this.measured; }
    measurements(): undefined { return undefined; }
    setGrid(size: Size): { kind: "applied"; size: Size } | { kind: "unchanged" } | { kind: "rejected"; reason: string } {
      if (size.columns < 2 || size.rows < 2) return { kind: "rejected", reason: `${size.columns}x${size.rows} is unusable` };
      if (this.grid.columns === size.columns && this.grid.rows === size.rows) return { kind: "unchanged" };
      this.grid = size;
      this.resizes.push(size);
      return { kind: "applied", size };
    }
    onInput(): () => void { return () => undefined; }
    onViewportChange(): () => void { return () => undefined; }
    focus(): void { this.focusCalls += 1; }
    hasSelection(): boolean { return false; }
    getSelection(): string { return ""; }
    search(): boolean { return false; }
    clearSearch(): void {}
    scrollToBottom(): void {}
    disposeGpuRenderer(): void {}
    dispose(): void { this.disposed = true; }
    async drainAndSerialize(): Promise<{ serialized: string; outputGeneration: number }> {
      // A wedged xterm write completion is what leaves the real renderer's
      // memoized drain pending forever, and the pane's next reveal waits on it.
      if (config.wedgeDrain) return new Promise<never>(() => undefined);
      return { serialized: "cached-screen", outputGeneration: 3 };
    }
    seed(bytes: Uint8Array, onRendered?: () => void): void {
      this.writes.push(`seed:${bytes.byteLength}`);
      if (onRendered) this.#pendingRendered.push(onRendered);
    }
    restore(serialized: string, onRendered?: () => void): boolean {
      this.restoredSerialized = serialized;
      this.restores.push(serialized);
      if (onRendered) this.#pendingRendered.push(onRendered);
      return true;
    }
    write(bytes: Uint8Array, onRendered?: () => void): void {
      this.writes.push(`write:${bytes.byteLength}`);
      if (onRendered) this.#pendingRendered.push(onRendered);
    }
    /** The real renderer reports rendered only after xterm drains its queue. */
    flushRendered(): void {
      const pending = this.#pendingRendered;
      this.#pendingRendered = [];
      for (const rendered of pending) rendered();
    }
  }
  return { FakeRenderer, renderers: { config, created: [] as InstanceType<typeof FakeRenderer>[] } };
});

vi.mock("./TerminalRenderer", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./TerminalRenderer")>()),
  XtermRenderer: class extends FakeRenderer {
    constructor() {
      super();
      renderers.created.push(this);
    }
  },
}));

import { TerminalPane } from "./TerminalPane";
import { prepareTerminalSnapshot, type TerminalEvent } from "./api";
import { terminalStateCache } from "./TerminalStateCache";
import { ownTerminalBytes } from "./TerminalBytes";
import type { PaneHealth, TerminalEventHub } from "./TerminalEventHub";

type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;

class FakeHub {
  generationEpoch: number | undefined = 7;
  rendered: Array<{ paneId: string; generation: number; terminalEpoch: number | undefined }> = [];
  seedRetries: string[] = [];
  #paneListeners = new Map<string, (event: PaneEvent) => void>();
  #healthListeners = new Map<string, (health: PaneHealth) => void>();
  #health = new Map<string, PaneHealth>();

  subscribePane(
    paneId: string,
    listener: (event: PaneEvent) => void,
    onHealthChange?: (health: PaneHealth) => void,
  ): () => void {
    this.#paneListeners.set(paneId, listener);
    if (onHealthChange) {
      this.#healthListeners.set(paneId, onHealthChange);
      onHealthChange(this.paneHealth(paneId));
    }
    return () => {
      this.#paneListeners.delete(paneId);
      this.#healthListeners.delete(paneId);
    };
  }

  paneHealth(paneId: string): PaneHealth {
    return this.#health.get(paneId) ?? { awaitingSeed: false, conflictReseedRequested: false };
  }

  /** Drives the hub's degraded-state surface the way `publish` would. */
  setPaneHealth(paneId: string, health: PaneHealth): void {
    this.#health.set(paneId, health);
    this.#healthListeners.get(paneId)?.(health);
  }

  retryPaneSeed(paneId: string): void {
    this.seedRetries.push(paneId);
    const health = this.paneHealth(paneId);
    if (health.conflictReseedRequested) this.setPaneHealth(paneId, { ...health, conflictReseedRequested: false });
  }

  #epochListeners = new Set<() => void>();

  subscribeEpoch(listener: () => void): () => void {
    this.#epochListeners.add(listener);
    return () => { this.#epochListeners.delete(listener); };
  }

  /** A reconnect: the host announces a new terminal epoch for the same pane. */
  advanceEpoch(): void {
    this.generationEpoch = (this.generationEpoch ?? 0) + 1;
    for (const listener of [...this.#epochListeners]) listener();
  }

  markRendered(paneId: string, generation: number, terminalEpoch?: number): void {
    this.rendered.push({ paneId, generation, terminalEpoch: terminalEpoch ?? this.generationEpoch });
  }

  visibilityCheckpoint(paneId: string): { terminalEpoch: number; outputGeneration: number } | undefined {
    void paneId;
    return this.generationEpoch === undefined ? undefined : { terminalEpoch: this.generationEpoch, outputGeneration: 0 };
  }

  deliver(event: PaneEvent): void {
    this.#paneListeners.get(event.paneId)?.(event);
  }

  asHub(): TerminalEventHub {
    return this as unknown as TerminalEventHub;
  }
}

function fixturePane(id: string): Pane {
  return {
    id, sessionId: "$1", windowId: "@1", index: 0, active: true,
    width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
  };
}

function seedEvent(paneId: string, generation = 1): PaneEvent {
  return {
    kind: "seed", paneId, generation, sequence: 1,
    data: ownTerminalBytes(new TextEncoder().encode("screen")),
  };
}

async function awaitPaint(): Promise<void> {
  // Matches afterNextPaint's two-frame convention, plus a settle for timers.
  await act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    await Promise.resolve();
  });
}

// The pane hides and reveals its terminal by writing an attribute straight to
// the DOM, so its host element has to be a real one rather than a stub.
const paneNodes: HTMLElement[] = [];
function paneNode(): HTMLElement {
  const node = paneNodes.at(-1);
  if (!node) throw new Error("No terminal pane element was mounted");
  return node;
}

const resizeCallbacks: Array<() => void> = [];

/** Reveals only: the hide half of the protocol passes `false` here. */
function revealCalls(): number {
  return api.setTerminalVisibility.mock.calls.filter((call) => call[2] === true).length;
}

function paneDiagnostic(mounted: ReactTestRenderer): string {
  return mounted.root
    .findAll((node) => node.props.className === "renderer-diagnostic")
    .flatMap((node) => node.children.filter((child): child is string => typeof child === "string"))
    .join("");
}

function awaitSeedResource(paneId: string, hostOwnsTheRequest: boolean): PaneEvent {
  return {
    kind: "paneResource", paneId, state: "released", requiresSeed: hostOwnsTheRequest,
    recoveryReason: "Host recovery pending", generation: 4, snapshotGeneration: 4,
    tailThroughGeneration: 4, sequence: 1,
    serializedSnapshot: ownTerminalBytes(new Uint8Array()),
    rawTail: ownTerminalBytes(new Uint8Array()),
  };
}

function paneElement(pane: Pane, hub: FakeHub, clientId: string, appFocused: boolean) {
  return <TerminalPane
    appFocused={appFocused}
    clientId={clientId}
    pane={pane}
    hub={hub.asHub()}
    onInput={() => undefined}
    onFocus={() => undefined}
    onMeasurements={() => undefined}
    onController={() => undefined}
  />;
}

async function mountPane(
  pane: Pane,
  hub: FakeHub,
  clientId = "client-a",
  appFocused = true,
): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(paneElement(pane, hub, clientId, appFocused), {
      createNodeMock: (element) => {
        const node = document.createElement("div");
        if ((element.props as Record<string, unknown>)["data-terminal-surface"]) paneNodes.push(node);
        return node;
      },
    });
  });
  return renderer;
}

/** A fresh tmux topology for an already-mounted pane. */
async function updatePane(mounted: ReactTestRenderer, pane: Pane, hub: FakeHub): Promise<void> {
  await act(async () => { mounted.update(paneElement(pane, hub, "client-a", true)); });
}

beforeEach(() => {
  resetPerfProbe();
  enablePerfProbe(async () => undefined);
  terminalStateCache.clear();
  renderers.created.length = 0;
  renderers.config.measured = undefined;
  renderers.config.wedgeDrain = false;
  paneNodes.length = 0;
  resizeCallbacks.length = 0;
  // Reset, not clear: a test that scripts a refusal must not leave it armed for
  // the next one, and a failing assertion skips any cleanup the test itself does.
  api.setTerminalVisibility.mockReset();
  api.requestTerminalSeed.mockReset();
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    ResizeObserver: class {
      constructor(callback: () => void) { resizeCallbacks.push(callback); }
      observe() {} unobserve() {} disconnect() {}
    },
  });
});
afterEach(() => {
  resetPerfProbe();
  terminalStateCache.clear();
  vi.useRealTimers();
});

describe("TerminalPane pane-paint span lifecycle", () => {
  it("restores the active pane keyboard target when the app returns to the foreground", async () => {
    const pane = fixturePane("%focus");
    const hub = new FakeHub();
    const mounted = await mountPane(pane, hub, "client-a", false);
    const renderer = renderers.created[0];
    expect(renderer.focusCalls).toBe(2);

    await act(async () => {
      mounted.update(<TerminalPane
        appFocused
        clientId="client-a"
        pane={pane}
        hub={hub.asHub()}
        onInput={() => undefined}
        onFocus={() => undefined}
        onMeasurements={() => undefined}
        onController={() => undefined}
      />);
    });

    expect(renderer.focusCalls).toBe(3);
    await act(async () => { mounted.unmount(); });
  });

  it("closes a create span when the freshly mounted pane paints its seed", async () => {
    const token = openPanePaintSpan("create.tab", "client-a");
    targetPanePaintSpan(token, "%9");

    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%9"), hub);
    await act(async () => { hub.deliver(seedEvent("%9")); });
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();

    expect(perfSummary().map(({ name }) => name)).toContain("create.tab");
    await act(async () => renderer.unmount());
  });

  it("closes a switch span when the pane restores from the local cache", async () => {
    terminalStateCache.set("%5", prepareTerminalSnapshot("warm-screen"), { terminalEpoch: 7, outputGeneration: 3 });
    const token = openPanePaintSpan("window.switch", "client-a");
    targetPanePaintSpan(token, "%5");

    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%5"), hub);
    expect(renderers.created[0].restoredSerialized).toBe("warm-screen");
    // Between `open` and the restored content xterm would paint an empty grid
    // with a cursor in it. The gate hides the terminal for exactly that gap —
    // and stays closed past the cache restore, because the reveal handshake is
    // about to answer with the same screen (see the redundant-restore tests).
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    await awaitPaint();

    // The span still closes: the cache restore did paint, it just did not show.
    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
  });

  it("does not reveal the deliberate blank a pane shows while it owes a seed", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%await"), hub);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // The blank RIS this branch writes is seed debt, not content. Showing it is
    // a whole extra visible repaint before the arriving seed paints the real
    // screen, so the gate stays shut and the 300ms fallback owns the worst case.
    await act(async () => {
      hub.deliver({
        kind: "paneResource", paneId: "%await", state: "released", requiresSeed: false,
        recoveryReason: "Renderer state was released", generation: 4, snapshotGeneration: 4,
        tailThroughGeneration: 4, sequence: 1,
        serializedSnapshot: ownTerminalBytes(new Uint8Array()),
        rawTail: ownTerminalBytes(new Uint8Array()),
      });
    });
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // The seed it was waiting for both repaints and reveals.
    await act(async () => { hub.deliver(seedEvent("%await", 5)); });
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    await act(async () => mounted.unmount());
  });

  it("reveals the terminal on the fallback timer when no content ever arrives", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%quiet"), hub);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // Nothing restores, nothing seeds, the host never answers. The safety net
    // is the only thing that can make this pane visible.
    act(() => { vi.advanceTimersByTime(300); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("closes a span whose acknowledgement arrives after the paint", async () => {
    const token = openPanePaintSpan("window.switch", "client-a");

    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%7"), hub);
    await act(async () => { hub.deliver(seedEvent("%7")); });
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();
    targetPanePaintSpan(token, "%7");

    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
  });

  it("closes a span when an already-mounted pane repaints from a fresh seed", async () => {
    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%4"), hub);
    await act(async () => { hub.deliver(seedEvent("%4")); });
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();
    expect(perfSummary()).toHaveLength(0);

    // The action lands while the pane is already mounted and painted; the
    // content it acknowledges arrives as a later seed on the same instance.
    const token = openPanePaintSpan("window.switch", "client-a");
    targetPanePaintSpan(token, "%4");
    await act(async () => { hub.deliver(seedEvent("%4", 2)); });
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();

    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
  });

  it("abandons a span when the user navigates away before the pane paints", async () => {
    const token = openPanePaintSpan("create.workspace", "client-a");
    targetPanePaintSpan(token, "%3");

    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%3"), hub);
    await act(async () => { hub.deliver(seedEvent("%3")); });
    // Unmounted before the seed painted. The teardown drain still completes
    // the parse afterwards, but those pixels were never shown.
    await act(async () => renderer.unmount());
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();
    expect(perfSummary().map(({ name }) => name)).not.toContain("create.workspace");

    abandonPanePaintSpansForScope("client-a");
    await awaitPaint();
    expect(perfSummary().map(({ name }) => name)).not.toContain("create.workspace");
  });
});

// A tab switch remounts every pane, and every remount used to be up to three
// full screen rewrites — each one an ESC c, so each one visibly blank before it
// filled. The cache restore and the reveal handshake carry the *same* bytes (the
// host stores this pane's hide checkpoint as `snapshot_generation` and hands it
// straight back), so the second rewrite is pure flicker. These pin one visible
// paint per switch: the cache restore paints without showing, and the handshake
// writes only what is new and reveals.
describe("TerminalPane reveal handshake", () => {
  function hostRestore(paneId: string, options: {
    serialized?: string;
    snapshotGeneration: number;
    tailThroughGeneration: number;
    tail?: string;
  }): PaneEvent {
    return {
      kind: "paneResource", paneId, state: "hiddenBuffered", requiresSeed: false,
      recoveryReason: "", generation: options.tailThroughGeneration,
      snapshotGeneration: options.snapshotGeneration,
      tailThroughGeneration: options.tailThroughGeneration, sequence: 2,
      serializedSnapshot: ownTerminalBytes(new TextEncoder().encode(options.serialized ?? "warm-screen")),
      rawTail: ownTerminalBytes(new TextEncoder().encode(options.tail ?? "")),
    };
  }

  async function mountWarmPane(paneId: string, hub: FakeHub) {
    terminalStateCache.set(paneId, prepareTerminalSnapshot("warm-screen"), { terminalEpoch: 7, outputGeneration: 3 });
    const mounted = await mountPane(fixturePane(paneId), hub);
    const renderer = renderers.created[0];
    expect(renderer.restores).toEqual(["warm-screen"]);
    await act(async () => { renderer.flushRendered(); });
    // Painted, acknowledged — and still hidden, because the handshake below is
    // about to answer with this very screen.
    expect(hub.rendered).toEqual([{ paneId, generation: 3, terminalEpoch: 7 }]);
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    return { mounted, renderer };
  }

  it("writes only the raw tail when the host hands back the screen the cache painted", async () => {
    const hub = new FakeHub();
    const { mounted, renderer } = await mountWarmPane("%warm", hub);

    await act(async () => {
      hub.deliver(hostRestore("%warm", { snapshotGeneration: 3, tailThroughGeneration: 5, tail: "tail" }));
    });
    // No second restore: it would clear the screen and rewrite it byte for byte.
    expect(renderer.restores).toEqual(["warm-screen"]);
    expect(renderer.writes).toEqual(["write:4"]);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    await act(async () => { renderer.flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    expect(hub.rendered.at(-1)).toEqual({ paneId: "%warm", generation: 5, terminalEpoch: 7 });
    await act(async () => mounted.unmount());
  });

  it("still acknowledges and reveals when the skipped restore has no tail at all", async () => {
    const hub = new FakeHub();
    const { mounted, renderer } = await mountWarmPane("%warm-notail", hub);

    await act(async () => {
      hub.deliver(hostRestore("%warm-notail", { snapshotGeneration: 3, tailThroughGeneration: 3 }));
    });
    expect(renderer.restores).toEqual(["warm-screen"]);
    // An empty write is still an ordered record, so its callback cannot overtake
    // bytes already inside xterm's parser.
    expect(renderer.writes).toEqual(["write:0"]);

    await act(async () => { renderer.flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    expect(hub.rendered.at(-1)).toEqual({ paneId: "%warm-notail", generation: 3, terminalEpoch: 7 });
    await act(async () => mounted.unmount());
  });

  it("restores when the host's snapshot is not the one the cache painted", async () => {
    const hub = new FakeHub();
    const { mounted, renderer } = await mountWarmPane("%stale", hub);

    // A newer host-side capture: its generation is not the cache's, so the two
    // screens are genuinely different and the restore has to happen.
    await act(async () => {
      hub.deliver(hostRestore("%stale", {
        serialized: "host-screen", snapshotGeneration: 4, tailThroughGeneration: 6, tail: "tail",
      }));
    });
    expect(renderer.restores).toEqual(["warm-screen", "host-screen"]);
    expect(renderer.writes).toEqual(["write:4"]);

    await act(async () => { renderer.flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    await act(async () => mounted.unmount());
  });

  it("reveals a cold pane on the restore the handshake brings it", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%cold"), hub);
    expect(renderers.created[0].restores).toEqual([]);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    await act(async () => {
      hub.deliver(hostRestore("%cold", { serialized: "host-screen", snapshotGeneration: 4, tailThroughGeneration: 4 }));
    });
    expect(renderers.created[0].restores).toEqual(["host-screen"]);
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    await act(async () => mounted.unmount());
  });

  it("reveals the pane the handshake had nothing to add to", async () => {
    // A reveal only reports local state the renderer has actually established,
    // so reaching this branch takes a reveal issued after the cache restore
    // landed — here the transport-not-up-yet retry.
    api.setTerminalVisibility.mockImplementationOnce(async () => { throw new Error("host bridge is disconnected"); });
    vi.useFakeTimers();
    const hub = new FakeHub();
    terminalStateCache.set("%agreed", prepareTerminalSnapshot("warm-screen"), { terminalEpoch: 7, outputGeneration: 3 });
    const mounted = await mountPane(fixturePane("%agreed"), hub);
    const renderer = renderers.created[0];
    await act(async () => { renderer.flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // Still inside the 300ms fallback, so the reveal below is the effect's.
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(revealCalls()).toBe(2);

    // The host kept no recovery material because this pane's own state is
    // already the screen. Nothing repaints, so this is the effect that reveals.
    await act(async () => {
      hub.deliver({
        kind: "paneResource", paneId: "%agreed", state: "visible", requiresSeed: false,
        recoveryReason: "", generation: 3, snapshotGeneration: 3, tailThroughGeneration: 3, sequence: 2,
        serializedSnapshot: ownTerminalBytes(new Uint8Array()),
        rawTail: ownTerminalBytes(new Uint8Array()),
      });
    });
    expect(renderer.restores).toEqual(["warm-screen"]);
    expect(paneNode().getAttribute("data-painted")).toBe("true");

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("falls back to the timer when the handshake never answers a warm pane", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    terminalStateCache.set("%silent", prepareTerminalSnapshot("warm-screen"), { terminalEpoch: 7, outputGeneration: 3 });
    const mounted = await mountPane(fixturePane("%silent"), hub);
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // Gating the reveal on the handshake may never leave a pane invisible.
    act(() => { vi.advanceTimersByTime(300); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("restores again when a seed has replaced what the cache painted", async () => {
    const hub = new FakeHub();
    const { mounted, renderer } = await mountWarmPane("%reseeded", hub);

    // An authoritative seed is now the screen, so the cache's generation says
    // nothing about what this terminal is showing.
    await act(async () => { hub.deliver(seedEvent("%reseeded", 4)); });
    await act(async () => { renderer.flushRendered(); });
    await act(async () => {
      hub.deliver(hostRestore("%reseeded", { snapshotGeneration: 3, tailThroughGeneration: 5, tail: "tail" }));
    });
    expect(renderer.restores).toEqual(["warm-screen", "warm-screen"]);
    await act(async () => mounted.unmount());
  });
});

// tmux owns a pane's grid, but its answer to a resize is a debounce plus a
// round trip away while the pane's box has already moved. These pin the handover
// in both directions: the box may lead only while tmux has not answered for it,
// and the moment tmux does, its numbers are what the terminal renders at.
describe("TerminalPane grid during a resize", () => {
  it("fits the terminal to its own box before tmux answers, then settles on tmux's grid", async () => {
    renderers.config.measured = { columns: 80, rows: 24 };
    const hub = new FakeHub();
    const pane = fixturePane("%grid");
    const mounted = await mountPane(pane, hub);
    const renderer = renderers.created[0];
    expect(renderer.resizes).toEqual([]);

    // The splitter moved. The box re-lays-out on this frame; tmux hears about
    // it only after the client-resize debounce and a host round trip.
    renderer.measured = { columns: 100, rows: 30 };
    act(() => { resizeCallbacks[0](); });
    expect(renderer.resizes).toEqual([{ columns: 100, rows: 30 }]);

    // tmux's snapshot lands on the same pixel box, so it agrees: the correcting
    // application is a no-op resize rather than a second reflow.
    await updatePane(mounted, { ...pane, width: 100, height: 30 }, hub);
    expect(renderer.resizes).toEqual([{ columns: 100, rows: 30 }]);

    // And the box is now the one tmux's grid was applied for, so further
    // observer callbacks re-apply tmux's grid and change nothing.
    act(() => { resizeCallbacks[0](); });
    act(() => { resizeCallbacks[0](); });
    expect(renderer.resizes).toEqual([{ columns: 100, rows: 30 }]);
    await act(async () => mounted.unmount());
  });

  it("lets a tmux snapshot that disagrees with the box win, and does not fight back", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    renderers.config.measured = { columns: 80, rows: 24 };
    const hub = new FakeHub();
    const pane = fixturePane("%split");
    const mounted = await mountPane(pane, hub);
    const renderer = renderers.created[0];

    renderer.measured = { columns: 50, rows: 24 };
    act(() => { resizeCallbacks[0](); });
    expect(renderer.resizes.at(-1)).toEqual({ columns: 50, rows: 24 });

    // tmux spends a column on the divider: 49 is what the program in the pane
    // addressed its cursor against, and it replaces the optimistic fit.
    await updatePane(mounted, { ...pane, width: 49, height: 24 }, hub);
    expect(renderer.resizes.at(-1)).toEqual({ columns: 49, rows: 24 });

    // The standing box/tmux divergence must never read as a box change: with
    // the box unmoved the observer keeps re-applying tmux's 49 forever.
    act(() => { resizeCallbacks[0](); });
    act(() => { resizeCallbacks[0](); });
    expect(renderer.resizes).toEqual([{ columns: 50, rows: 24 }, { columns: 49, rows: 24 }]);
    await act(async () => mounted.unmount());
    warn.mockRestore();
  });
});

// Every degraded state a pane can be latched into has exactly one recovery
// signal — one seed request, one reseed, one reveal — and no time bound. When
// that signal is lost the pane is frozen until it is remounted, which is the
// "one terminal pane frozen forever" bug. These pin the bound.
describe("TerminalPane degraded-state watchdog", () => {
  it("keeps asking for the seed a stuck pane is waiting for, with backoff", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%stuck"), hub);
    expect(revealCalls()).toBe(1);

    // `requiresSeed` means the host owns the request, so nothing on this side
    // ever asks. A request the host suppressed used to end the story here.
    await act(async () => { hub.deliver(awaitSeedResource("%stuck", true)); });
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);
    // A seed cannot reach a pane the host believes is hidden, so the retry
    // re-asserts visibility as well.
    expect(revealCalls()).toBe(2);
    expect(paneDiagnostic(mounted)).toContain("retrying");

    // The second retry is four seconds after the first, not two.
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(2);
    expect(perfCounterSnapshot()["terminal.pane.watchdogReseeds"]).toBe(2);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("stops retrying when a seed lands and gives the next fault a short first delay", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%recover"), hub);
    await act(async () => {
      hub.setPaneHealth("%recover", { awaitingSeed: true, conflictReseedRequested: false });
    });
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);

    await act(async () => {
      hub.setPaneHealth("%recover", { awaitingSeed: false, conflictReseedRequested: false });
      hub.deliver(seedEvent("%recover"));
    });
    // Healthy: no timer is left running, whatever the backoff had reached.
    await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);

    // And the next episode starts at two seconds rather than inheriting the
    // previous one's cadence.
    await act(async () => {
      hub.setPaneHealth("%recover", { awaitingSeed: true, conflictReseedRequested: false });
    });
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(2);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("reopens the hub's one-shot conflict latch as part of retrying it", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%conflict"), hub);
    await act(async () => {
      hub.setPaneHealth("%conflict", { awaitingSeed: false, conflictReseedRequested: true });
    });

    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(hub.seedRetries).toEqual(["%conflict"]);
    expect(hub.paneHealth("%conflict").conflictReseedRequested).toBe(false);
    // Withdrawing the hub's reason does not end the episode: the seed this
    // just asked for has still not arrived.
    await act(async () => { await vi.advanceTimersByTimeAsync(4_000); });
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(2);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("reveals a remounted pane once a wedged handoff passes its bound", async () => {
    renderers.config.wedgeDrain = true;
    const hub = new FakeHub();
    const first = await mountPane(fixturePane("%wedge"), hub);
    expect(revealCalls()).toBe(1);
    // The drain this unmount starts never completes, so the handoff the next
    // reveal serializes behind stays pending forever.
    await act(async () => first.unmount());

    vi.useFakeTimers();
    const second = await mountPane(fixturePane("%wedge"), hub);
    expect(revealCalls()).toBe(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(revealCalls()).toBe(2);
    expect(perfCounterSnapshot()["terminal.pane.handoffTimeouts"]).toBe(1);

    vi.useRealTimers();
    await act(async () => second.unmount());
  });

  it("re-asserts visibility the host refused instead of only clearing its latch", async () => {
    api.setTerminalVisibility.mockImplementationOnce(async () => { throw new Error("visibility conflict"); });
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%refused"), hub);
    // The pre-existing recovery: one seed request, and nothing that re-runs
    // the reveal itself.
    expect(revealCalls()).toBe(1);
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);

    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(revealCalls()).toBe(2);
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(2);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  // A connection (re)start rejects the first reveals it is given: the client id
  // comes back before the SSH handshake, and the bridge announces its epoch a
  // round trip before it can carry a request. Falling straight through to the
  // watchdog leaves the visible pane frozen for seconds on every rebuild.
  it("retries a reveal the host refused because its transport was not up yet", async () => {
    api.setTerminalVisibility.mockImplementationOnce(async () => {
      throw new Error("host bridge is disconnected");
    });
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%early"), hub);
    expect(revealCalls()).toBe(1);
    // Nothing about this failure is a conflict, so none of the conflict
    // recovery runs for it.
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(revealCalls()).toBe(2);
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("gives up on a transient reveal once its retries run out", async () => {
    api.setTerminalVisibility.mockImplementation(async (...args: unknown[]) => {
      if (args[2] !== true) return undefined;
      throw new Error("terminal client is no longer attached");
    });
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%hopeless"), hub);

    // Eight retries at 250ms, and then the watchdog's own re-assertion at +2s
    // takes over rather than this looping forever.
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(revealCalls()).toBe(9);
    expect(api.requestTerminalSeed).toHaveBeenCalledTimes(1);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("lets a superseded reveal fail without marking the pane degraded", async () => {
    let refuseFirstReveal!: (error: Error) => void;
    api.setTerminalVisibility.mockImplementationOnce(
      () => new Promise<undefined>((_, reject) => { refuseFirstReveal = reject; }),
    );
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%superseded"), hub);
    expect(revealCalls()).toBe(1);

    // The reconnect supersedes the in-flight reveal, and its own reveal lands.
    await act(async () => { hub.advanceEpoch(); });
    expect(revealCalls()).toBe(2);

    vi.useFakeTimers();
    await act(async () => { refuseFirstReveal(new Error("visibility conflict")); });
    // The pane is healthy; the loser of that race has no standing to say
    // otherwise, so no watchdog episode starts on its behalf.
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(revealCalls()).toBe(2);
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });

  it("arms no timer for a pane that is working", async () => {
    vi.useFakeTimers();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%healthy"), hub);
    await act(async () => { hub.deliver(seedEvent("%healthy")); });
    await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();
    expect(revealCalls()).toBe(1);

    vi.useRealTimers();
    await act(async () => mounted.unmount());
  });
});
