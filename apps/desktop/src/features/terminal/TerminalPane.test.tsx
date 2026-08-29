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
  requestTerminalHistory: vi.fn(async (..._args: unknown[]) => undefined),
}));
vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  setTerminalVisibility: api.setTerminalVisibility,
  requestTerminalSeed: api.requestTerminalSeed,
  requestTerminalHistory: api.requestTerminalHistory,
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
    measured = config.measured;
    /** Mirrors the real renderer: `setGrid` is the only writer of cols/rows. */
    grid: Size = { columns: 80, rows: 24 };
    resizes: Size[] = [];
    fontSizes: number[] = [];
    #pendingRendered: Array<() => void> = [];
    #measurementListeners = new Set<() => void>();
    #topListeners = new Set<() => void>();
    /** What every splice this pane attempts is answered with. */
    historyOutcome: "applied" | "superseded" = "applied";
    historySplices: Array<{ bytes: number; throughGeneration: number }> = [];
    enqueuedGeneration = 0;

    open(): void {}
    measure(): Size | undefined { return this.measured; }
    measurements(): undefined { return undefined; }
    onMeasurementsChange(listener: () => void): () => void {
      this.#measurementListeners.add(listener);
      return () => { this.#measurementListeners.delete(listener); };
    }
    emitMeasurementsChange(): void {
      for (const listener of this.#measurementListeners) listener();
    }
    setFontSize(fontSize: number): void { this.fontSizes.push(fontSize); }
    setGrid(size: Size): { kind: "applied"; size: Size } | { kind: "unchanged" } | { kind: "rejected"; reason: string } {
      if (size.columns < 2 || size.rows < 2) return { kind: "rejected", reason: `${size.columns}x${size.rows} is unusable` };
      if (this.grid.columns === size.columns && this.grid.rows === size.rows) return { kind: "unchanged" };
      this.grid = size;
      this.resizes.push(size);
      return { kind: "applied", size };
    }
    onInput(): () => void { return () => undefined; }
    onSelectionChange(): () => void { return () => undefined; }
    onViewportChange(): () => void { return () => undefined; }
    onScrollbackTopReached(listener: () => void): () => void {
      this.#topListeners.add(listener);
      return () => { this.#topListeners.delete(listener); };
    }
    /** The user scrolling up against the top of this pane. */
    reachTop(): void {
      for (const listener of this.#topListeners) listener();
    }
    prependHistory(history: Uint8Array, throughGeneration: number): "applied" | "superseded" {
      this.historySplices.push({ bytes: history.byteLength, throughGeneration });
      return this.historyOutcome;
    }
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
    seed(bytes: Uint8Array, onRendered?: () => void, generation = 0): void {
      this.writes.push(`seed:${bytes.byteLength}`);
      this.enqueuedGeneration = generation;
      if (onRendered) this.#pendingRendered.push(onRendered);
    }
    restore(serialized: string, onRendered?: () => void): boolean {
      this.restoredSerialized = serialized;
      if (onRendered) this.#pendingRendered.push(onRendered);
      return true;
    }
    write(bytes: Uint8Array, onRendered?: () => void, generation = 0): boolean {
      this.writes.push(`write:${bytes.byteLength}`);
      if (generation > this.enqueuedGeneration) this.enqueuedGeneration = generation;
      if (onRendered) this.#pendingRendered.push(onRendered);
      return true;
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
import { type TerminalEvent } from "./api";
import { terminalStateCache } from "./TerminalStateCache";
import { ownTerminalBytes } from "./TerminalBytes";
import type { PaneHealth, TerminalEventHub } from "./TerminalEventHub";

type PaneEvent = Extract<TerminalEvent, { kind: "seed" | "output" | "paneResource" | "seedDiagnostic" | "terminalHistory" }>;

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
    resumeFromRenderer: false,
    recoveryReason: "Host recovery pending", generation: 4, snapshotGeneration: 4,
    tailThroughGeneration: 4, sequence: 1,
    serializedSnapshot: ownTerminalBytes(new Uint8Array()),
    rawTail: ownTerminalBytes(new Uint8Array()),
  };
}

function paneElement(pane: Pane, hub: FakeHub, clientId: string, appFocused: boolean, terminalFontSize = 13) {
  return <TerminalPane
    appFocused={appFocused}
    clientId={clientId}
    pane={pane}
    hub={hub.asHub()}
    onInput={() => undefined}
    onFocus={() => undefined}
    onMeasurements={() => undefined}
    onController={() => undefined}
    terminalFontSize={terminalFontSize}
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
  api.requestTerminalHistory.mockReset();
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
  it("updates font size on the existing renderer", async () => {
    const pane = fixturePane("%font");
    const hub = new FakeHub();
    const mounted = await mountPane(pane, hub);
    const renderer = renderers.created[0];
    expect(renderer.fontSizes.at(-1)).toBe(13);

    await act(async () => { mounted.update(paneElement(pane, hub, "client-a", true, 18)); });

    expect(renderers.created).toHaveLength(1);
    expect(renderer.fontSizes.at(-1)).toBe(18);
    await act(async () => { mounted.unmount(); });
  });

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
    terminalStateCache.set("%5", "warm-screen", { terminalEpoch: 7, outputGeneration: 3 });
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

  it("does not reveal the deliberate blank a pane shows while it owes a seed", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%await"), hub);
    expect(paneNode().getAttribute("data-painted")).toBe("false");

    // The blank RIS this branch writes is seed debt, not content. Showing it is
    // a whole extra visible repaint before the arriving seed paints the real
    // screen, and the diagnostic banner tells the user what is happening
    // whether or not the terminal itself is visible.
    await act(async () => {
      hub.deliver({
        kind: "paneResource", paneId: "%await", state: "released", requiresSeed: false,
        resumeFromRenderer: false,
        recoveryReason: "Renderer state was released", generation: 4, snapshotGeneration: 4,
        tailThroughGeneration: 4, sequence: 1,
        serializedSnapshot: ownTerminalBytes(new Uint8Array()),
        rawTail: ownTerminalBytes(new Uint8Array()),
      });
    });
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    expect(paneDiagnostic(mounted)).toContain("waiting for a fresh terminal seed");

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

// tmux owns a pane's grid, but its answer to a resize is a debounce plus a
// round trip away while the pane's box has already moved. These pin the handover
// in both directions: the box may lead only while tmux has not answered for it,
// and the moment tmux does, its numbers are what the terminal renders at.
describe("TerminalPane grid during a resize", () => {
  it("refits when xterm metrics change without a CSS-box resize", async () => {
    renderers.config.measured = { columns: 80, rows: 24 };
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%dpr"), hub);
    const renderer = renderers.created[0];

    // Moving a fixed-size window between displays lets xterm recompute its
    // device-pixel-rounded cell while ResizeObserver has nothing to report.
    renderer.measured = { columns: 79, rows: 23 };
    act(() => { renderer.emitMeasurementsChange(); });
    expect(renderer.resizes).toEqual([{ columns: 79, rows: 23 }]);

    await act(async () => mounted.unmount());
    renderer.measured = { columns: 78, rows: 22 };
    act(() => { renderer.emitMeasurementsChange(); });
    expect(renderer.resizes).toEqual([{ columns: 79, rows: 23 }]);
  });

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

/**
 * A seed is the visible grid and nothing above it, so the scrollback the user
 * scrolls up looking for is still in tmux. Reaching the top is the request for
 * it — there is no button, because the gesture already says what a button
 * would.
 */
describe("lazy scrollback", () => {
  function historyEvent(paneId: string, text: string): PaneEvent {
    return {
      kind: "terminalHistory", paneId, sequence: 2,
      data: ownTerminalBytes(new TextEncoder().encode(text)),
    };
  }

  it("asks once when a screen-seeded pane is scrolled to the top, and not again while the ask is outstanding", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h1"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h1", 4)); });

    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h1", 2000]]);

    // The answer has not arrived, so the second and third gestures are the same
    // question and cost nothing.
    await act(async () => { renderer.reachTop(); renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);

    await act(async () => { hub.deliver(historyEvent("%h1", "earlier output")); });
    // Quoted back so the renderer can refuse a splice onto a stream that moved.
    expect(renderer.historySplices).toEqual([{ bytes: 14, throughGeneration: 4 }]);

    // Loaded is loaded: this pane's scrollback is now on the terminal.
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("asks again after a splice the stream moved out from under", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h2"), hub);
    const renderer = renderers.created[0];
    renderer.historyOutcome = "superseded";
    await act(async () => { hub.deliver(seedEvent("%h2", 4)); });

    await act(async () => { renderer.reachTop(); });
    await act(async () => { hub.deliver(historyEvent("%h2", "earlier output")); });
    expect(renderer.historySplices).toHaveLength(1);

    // Nothing was applied, so the question is still open and the next gesture
    // asks it again — against the screen the user is now looking at.
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(2);
    await act(async () => { mounted.unmount(); });
  });

  it("takes an empty answer as the whole answer", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h3"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h3", 4)); });

    await act(async () => { renderer.reachTop(); });
    await act(async () => { hub.deliver(historyEvent("%h3", "")); });

    // There is nothing above this screen, so nothing is spliced and nothing is
    // asked for again.
    expect(renderer.historySplices).toEqual([]);
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("never asks for a pane that came up from its own cached screen", async () => {
    terminalStateCache.set("%h4", "warm-screen", { terminalEpoch: 7, outputGeneration: 3 });
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h4"), hub);
    const renderer = renderers.created[0];
    expect(renderer.restoredSerialized).toBe("warm-screen");

    // That screen is a serialization of this pane's own buffer, scrollback
    // included: there is nothing above it the host is holding.
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();
    await act(async () => { mounted.unmount(); });
  });
});
