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
  perfSummary,
  resetPerfProbe,
  targetPanePaintSpan,
} from "../../perf/probe";

const api = vi.hoisted(() => ({
  setTerminalVisibility: vi.fn(async () => undefined),
  requestTerminalSeed: vi.fn(async () => undefined),
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
  const config: { measured?: Size } = {};
  class FakeRenderer {
    writes: string[] = [];
    disposed = false;
    focusCalls = 0;
    restoredSerialized: string | undefined;
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
      return { serialized: "cached-screen", outputGeneration: 3 };
    }
    seed(bytes: Uint8Array, onRendered?: () => void): void {
      this.writes.push(`seed:${bytes.byteLength}`);
      if (onRendered) this.#pendingRendered.push(onRendered);
    }
    restore(serialized: string, onRendered?: () => void): boolean {
      this.restoredSerialized = serialized;
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
import type { TerminalEventHub } from "./TerminalEventHub";

type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" }>;

class FakeHub {
  generationEpoch: number | undefined = 7;
  rendered: Array<{ paneId: string; generation: number; terminalEpoch: number | undefined }> = [];
  #paneListeners = new Map<string, (event: PaneEvent) => void>();

  subscribePane(paneId: string, listener: (event: PaneEvent) => void): () => void {
    this.#paneListeners.set(paneId, listener);
    return () => this.#paneListeners.delete(paneId);
  }

  subscribeEpoch(): () => void { return () => undefined; }

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
  paneNodes.length = 0;
  resizeCallbacks.length = 0;
  api.setTerminalVisibility.mockClear();
  api.requestTerminalSeed.mockClear();
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
    // with a cursor in it. The gate hides the terminal for exactly that gap,
    // and the restore's rendered callback is what ends it.
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    await awaitPaint();

    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
  });

  it("reveals a pane that seeds empty and waits for a fresh seed", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%await"), hub);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // An empty screen under a diagnostic is this branch's intended visible
    // state; it produces no rendered callback, so it must reveal itself.
    await act(async () => {
      hub.deliver({
        kind: "paneResource", paneId: "%await", state: "released", requiresSeed: false,
        recoveryReason: "Renderer state was released", generation: 4, snapshotGeneration: 4,
        tailThroughGeneration: 4, sequence: 1,
        serializedSnapshot: ownTerminalBytes(new Uint8Array()),
        rawTail: ownTerminalBytes(new Uint8Array()),
      });
    });

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
