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
    restoredViewport: { atBottom: boolean; viewportLine: number; grid: Size } | undefined;
    measured = config.measured;
    /** Mirrors the real renderer: `setGrid` is the only writer of cols/rows. */
    grid: Size = { columns: 80, rows: 24 };
    resizes: Size[] = [];
    fontSizes: number[] = [];
    #pendingRendered: Array<() => void> = [];
    #measurementListeners = new Set<() => void>();
    #topListeners = new Set<() => void>();
    #gridListeners = new Set<() => void>();
    #inputListeners = new Set<(input: { kind: "text"; data: string }) => void>();
    #viewportListeners = new Set<(state: { atBottom: boolean; newOutput: boolean }) => void>();
    viewport = { atBottom: true, newOutput: false };
    unrenderedOutput = 0;
    scrollBottomCalls = 0;
    /** What every splice this pane attempts is answered with. */
    historyOutcome: "applied" | "superseded" = "applied";
    historySplices: Array<{ bytes: number; skip: number; columns: number; rows: number }> = [];
    /** Rows above the screen, as the real renderer counts them. */
    scrollbackRows = 0;
    /** The ceiling `scrollbackRows` walks up to, as xterm's `scrollback` sets it. */
    scrollbackLimit = 10_000;
    /** A TUI is drawing: there is no scrollback to prepend to. */
    alternateScreen = false;

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
      for (const listener of this.#gridListeners) listener();
      return { kind: "applied", size };
    }
    restoreViewport(viewport: { atBottom: boolean; viewportLine: number; grid: Size }): void {
      this.restoredViewport = viewport;
    }
    onGridApplied(listener: () => void): () => void {
      this.#gridListeners.add(listener);
      return () => { this.#gridListeners.delete(listener); };
    }
    isAlternateScreenActive(): boolean { return this.alternateScreen; }
    isApplicationCursorMode(): boolean { return false; }
    onInput(listener: (input: { kind: "text"; data: string }) => void): () => void {
      this.#inputListeners.add(listener);
      return () => { this.#inputListeners.delete(listener); };
    }
    emitInput(data: string): void {
      for (const listener of this.#inputListeners) listener({ kind: "text", data });
    }
    onSelectionChange(): () => void { return () => undefined; }
    onViewportChange(listener: (state: { atBottom: boolean; newOutput: boolean }) => void): () => void {
      this.#viewportListeners.add(listener);
      listener(this.viewport);
      return () => { this.#viewportListeners.delete(listener); };
    }
    emitViewport(atBottom: boolean, newOutput = this.viewport.newOutput): void {
      this.viewport = { atBottom, newOutput };
      for (const listener of this.#viewportListeners) listener(this.viewport);
    }
    onScrollbackTopReached(listener: () => void): () => void {
      this.#topListeners.add(listener);
      return () => { this.#topListeners.delete(listener); };
    }
    /** The user scrolling up against the top of this pane. */
    reachTop(): void {
      for (const listener of this.#topListeners) listener();
    }
    async prependHistory(
      history: Uint8Array,
      anchor: { skip: number; columns: number; rows: number },
    ): Promise<"applied" | "superseded"> {
      this.historySplices.push({ bytes: history.byteLength, ...anchor });
      return this.historyOutcome;
    }
    focus(): void { this.focusCalls += 1; }
    hasSelection(): boolean { return false; }
    getSelection(): string { return ""; }
    search(): boolean { return false; }
    clearSearch(): void {}
    scrollToBottom(): void {
      this.scrollBottomCalls += 1;
      this.emitViewport(true, false);
    }
    noteUnrenderedOutput(): void {
      this.unrenderedOutput += 1;
      this.emitViewport(false, true);
    }
    disposeGpuRenderer(): void {}
    dispose(): void { this.disposed = true; }
    async drainAndSerialize(): Promise<{ serialized: string; outputGeneration: number; viewport: { atBottom: boolean; viewportLine: number; grid: Size } }> {
      // A wedged xterm write completion is what leaves the real renderer's
      // memoized drain pending forever, and the pane's next reveal waits on it.
      if (config.wedgeDrain) return new Promise<never>(() => undefined);
      return {
        serialized: "cached-screen",
        outputGeneration: 3,
        viewport: { atBottom: false, viewportLine: 4, grid: this.grid },
      };
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
    write(bytes: Uint8Array, onRendered?: () => void): boolean {
      this.writes.push(`write:${bytes.byteLength}`);
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
import { terminalCacheKey, terminalStateCache } from "./TerminalStateCache";
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

function outputEvent(paneId: string, generation: number, value = "output"): PaneEvent {
  return {
    kind: "output", paneId, generation, sequence: generation,
    data: ownTerminalBytes(new TextEncoder().encode(value)),
  };
}

function resumeEvent(
  paneId: string,
  snapshotGeneration: number,
  tailThroughGeneration: number,
  tail: string,
): PaneEvent {
  return {
    kind: "paneResource", paneId, state: "visible", requiresSeed: false,
    resumeFromRenderer: true, recoveryReason: "", generation: tailThroughGeneration,
    snapshotGeneration, tailThroughGeneration, sequence: tailThroughGeneration,
    rawTail: ownTerminalBytes(new TextEncoder().encode(tail)),
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
    rawTail: ownTerminalBytes(new Uint8Array()),
  };
}

function paneElement(
  pane: Pane,
  hub: FakeHub,
  clientId: string,
  appFocused: boolean,
  terminalFontSize = 13,
  onInput: (paneId: string, input: { kind: "text"; data: string } | { kind: "binary"; data: Uint8Array }) => void = () => undefined,
  activity: { onKeyActivity?(paneId: string): void; onPointerActivity?(paneId: string): void } = {},
) {
  return <TerminalPane
    appFocused={appFocused}
    cacheScope="local"
    clientId={clientId}
    pane={pane}
    hub={hub.asHub()}
    onInput={onInput}
    onKeyActivity={activity.onKeyActivity}
    onPointerActivity={activity.onPointerActivity}
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
  onInput?: (paneId: string, input: { kind: "text"; data: string } | { kind: "binary"; data: Uint8Array }) => void,
  activity?: { onKeyActivity?(paneId: string): void; onPointerActivity?(paneId: string): void },
): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(paneElement(pane, hub, clientId, appFocused, 13, onInput, activity), {
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
  it("reports deliberate pointer and physical-key activity without using focus", async () => {
    const pane = fixturePane("%activity");
    const hub = new FakeHub();
    const onKeyActivity = vi.fn();
    const onPointerActivity = vi.fn();
    const mounted = await mountPane(pane, hub, "client-a", true, undefined, { onKeyActivity, onPointerActivity });
    const surface = mounted.root.findByProps({ "data-terminal-surface": "true" });
    await act(async () => surface.props.onMouseDownCapture({
      currentTarget: { dataset: {} },
      nativeEvent: { shiftKey: false },
    }));
    expect(onPointerActivity).toHaveBeenCalledWith("%activity");
    expect(onKeyActivity).not.toHaveBeenCalled();

    await act(async () => { paneNodes[0].dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true })); });
    expect(onKeyActivity).toHaveBeenCalledWith("%activity");
    await act(async () => mounted.unmount());
  });

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
        cacheScope="local"
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
    terminalStateCache.set(terminalCacheKey("local", "%5"), "warm-screen", {
      checkpoint: { terminalEpoch: 7, outputGeneration: 3 },
      viewport: { atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 } },
    });
    const token = openPanePaintSpan("window.switch", "client-a");
    targetPanePaintSpan(token, "%5");

    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%5"), hub);
    expect(renderers.created[0].restoredSerialized).toBe("warm-screen");
    // Between `open` and the restored content xterm would paint an empty grid
    // with a cursor in it. The gate hides the terminal for exactly that gap,
    // and the restore's rendered callback is what ends it.
    expect(paneNode().getAttribute("data-painted")).toBe("false");
    expect(renderers.created[0].restoredViewport).toBeUndefined();
    await act(async () => { renderers.created[0].flushRendered(); });
    expect(renderers.created[0].restoredViewport).toEqual({
      atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 },
    });
    expect(paneNode().getAttribute("data-painted")).toBe("true");
    await awaitPaint();

    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
  });

  // `%5` exists on every tmux server; a screen another host cached under it
  // is not this pane's screen.
  it("does not restore a screen another host cached under the same pane id", async () => {
    const viewport = { atBottom: true, viewportLine: 0, grid: { columns: 80, rows: 24 } };
    const checkpoint = { terminalEpoch: 7, outputGeneration: 3 };
    terminalStateCache.set(terminalCacheKey("remote", "%5"), "remote-screen", { checkpoint, viewport });
    // An unscoped key is what a call that forgot the scope would read.
    terminalStateCache.set("%5", "unscoped-screen", { checkpoint, viewport });
    const hub = new FakeHub();
    const renderer = await mountPane(fixturePane("%5"), hub);
    expect(renderers.created[0].restoredSerialized).toBeUndefined();
    expect(terminalStateCache.get(terminalCacheKey("remote", "%5"))?.serialized).toBe("remote-screen");
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
 * scrolls up looking for is still in tmux. It comes back a page at a time, the
 * way tmux's own copy-mode reads it: reaching the top is the request for the
 * next page — there is no button, because the gesture already says what a
 * button would — and the first page is fetched behind the pane's first paint so
 * a wheel a moment after a switch finds something already there.
 */
describe("tmux-style terminal reading", () => {
  it("keeps live output out of the historical xterm and refreshes once at the bottom", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read1"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%read1", 1)); renderer.flushRendered(); });
    api.requestTerminalHistory.mockClear();

    await act(async () => { renderer.emitViewport(false); });
    await act(async () => {
      hub.deliver(outputEvent("%read1", 2, "first"));
      hub.deliver(outputEvent("%read1", 3, "second"));
    });

    expect(renderer.writes.filter((write) => write.startsWith("write:"))).toEqual([]);
    expect(renderer.unrenderedOutput).toBe(1);
    expect(hub.rendered.map((entry) => entry.generation)).not.toContain(2);
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();

    await act(async () => { renderer.emitViewport(true); });
    expect(api.requestTerminalSeed.mock.calls).toEqual([["client-a", "%read1"]]);
    expect(renderer.scrollBottomCalls).toBe(0);

    // Output racing the seed uses the existing bounded recovery queue rather
    // than touching the historical screen.
    await act(async () => { hub.deliver(outputEvent("%read1", 4, "deferred")); });
    expect(renderer.writes.filter((write) => write.startsWith("write:"))).toEqual([]);

    await act(async () => { hub.deliver(seedEvent("%read1", 5)); renderer.flushRendered(); });
    await act(async () => { hub.deliver(outputEvent("%read1", 6, "live")); renderer.flushRendered(); });
    expect(renderer.writes).toContain("write:4");
    expect(hub.rendered.map((entry) => entry.generation)).toContain(6);
    await act(async () => { mounted.unmount(); });
  });

  it("returns immediately when clean and sends input once while a stale screen refreshes", async () => {
    const inputs = vi.fn();
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read2"), hub, "client-a", true, inputs);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%read2", 1)); renderer.flushRendered(); });

    await act(async () => { renderer.emitViewport(false); renderer.emitInput("a"); });
    expect(renderer.scrollBottomCalls).toBe(1);
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();
    expect(inputs.mock.calls).toEqual([["%read2", { kind: "text", data: "a" }]]);

    await act(async () => { renderer.emitViewport(false); });
    await act(async () => { hub.deliver(outputEvent("%read2", 2)); });
    await act(async () => { renderer.emitInput("b"); renderer.emitInput("c"); });

    expect(api.requestTerminalSeed.mock.calls).toEqual([["client-a", "%read2"]]);
    expect(inputs.mock.calls.slice(1)).toEqual([
      ["%read2", { kind: "text", data: "b" }],
      ["%read2", { kind: "text", data: "c" }],
    ]);
    await act(async () => { mounted.unmount(); });
  });

  it("never caches or claims an outdated reading screen during hide", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read3"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%read3", 1)); renderer.flushRendered(); });
    await act(async () => { renderer.emitViewport(false); hub.deliver(outputEvent("%read3", 2)); });

    await act(async () => { mounted.unmount(); await Promise.resolve(); });

    expect(terminalStateCache.get(terminalCacheKey("local", "%read3"))).toBeUndefined();
    const hide = api.setTerminalVisibility.mock.calls.find((call) => call[2] === false);
    expect(hide?.[3]).toBe(false);
  });

  it("leaves the ordinary live-output path unchanged at the bottom", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read4"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%read4", 1)); renderer.flushRendered(); });

    await act(async () => { hub.deliver(outputEvent("%read4", 2, "live")); renderer.flushRendered(); });

    expect(renderer.writes).toContain("write:4");
    expect(renderer.unrenderedOutput).toBe(0);
    expect(api.requestTerminalSeed).not.toHaveBeenCalled();
    await act(async () => { mounted.unmount(); });
  });

  it("does not let a late cached-resume tail repaint a restored reading viewport", async () => {
    terminalStateCache.set(terminalCacheKey("local", "%read5"), "warm-screen", {
      checkpoint: { terminalEpoch: 7, outputGeneration: 3 },
      viewport: { atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 } },
    });
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read5"), hub);
    const renderer = renderers.created[0];
    await act(async () => { renderer.flushRendered(); renderer.emitViewport(false); });

    // This output races the reveal and would normally wait in the bounded
    // recovery queue; the reveal's own tail covers another hidden-time write.
    await act(async () => { hub.deliver(outputEvent("%read5", 4, "deferred")); });
    await act(async () => { hub.deliver(resumeEvent("%read5", 3, 5, "tail")); });

    expect(renderer.writes.filter((write) => write.startsWith("write:"))).toEqual([]);
    expect(renderer.unrenderedOutput).toBe(1);
    await act(async () => { renderer.emitInput("x"); });
    expect(api.requestTerminalSeed.mock.calls).toEqual([["client-a", "%read5"]]);
    await act(async () => { mounted.unmount(); });
  });

  it("keeps one seed and the old pixels when return-live outruns a cached-resume answer", async () => {
    terminalStateCache.set(terminalCacheKey("local", "%read7"), "warm-screen", {
      checkpoint: { terminalEpoch: 7, outputGeneration: 3 },
      viewport: { atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 } },
    });
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read7"), hub);
    const renderer = renderers.created[0];
    await act(async () => { renderer.flushRendered(); renderer.emitViewport(false); });
    await act(async () => { hub.deliver(outputEvent("%read7", 4, "deferred")); });
    await act(async () => { renderer.emitInput("x"); });
    await act(async () => { hub.deliver(resumeEvent("%read7", 3, 5, "late-tail")); });

    expect(api.requestTerminalSeed.mock.calls).toEqual([["client-a", "%read7"]]);
    expect(renderer.writes.filter((write) => write.startsWith("seed:") || write.startsWith("write:"))).toEqual([]);

    await act(async () => { hub.deliver(seedEvent("%read7", 6)); renderer.flushRendered(); });
    expect(renderer.writes).toContain("seed:6");
    await act(async () => { mounted.unmount(); });
  });

  it("does not flush queued reveal output through a non-resume readiness answer", async () => {
    terminalStateCache.set(terminalCacheKey("local", "%read8"), "warm-screen", {
      checkpoint: { terminalEpoch: 7, outputGeneration: 3 },
      viewport: { atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 } },
    });
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read8"), hub);
    const renderer = renderers.created[0];
    await act(async () => { renderer.flushRendered(); });
    await act(async () => { hub.deliver(outputEvent("%read8", 4, "queued")); });
    await act(async () => { renderer.emitViewport(false); });
    await act(async () => {
      hub.deliver({
        kind: "paneResource", paneId: "%read8", state: "visible", requiresSeed: false,
        resumeFromRenderer: false, recoveryReason: "", generation: 4,
        snapshotGeneration: 3, tailThroughGeneration: 4, sequence: 5,
        rawTail: ownTerminalBytes(new Uint8Array()),
      });
    });

    expect(renderer.writes.filter((write) => write.startsWith("write:"))).toEqual([]);
    expect(renderer.unrenderedOutput).toBe(1);
    await act(async () => { mounted.unmount(); });
  });

  it("drops a history page already in flight when live output makes the view outdated", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%read6"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%read6", 1)); renderer.flushRendered(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);

    await act(async () => { renderer.emitViewport(false); hub.deliver(outputEvent("%read6", 2)); });
    await act(async () => {
      hub.deliver({
        kind: "terminalHistory", paneId: "%read6", sequence: 3, historySize: 2_000,
        data: ownTerminalBytes(new TextEncoder().encode("earlier output")),
      });
    });

    expect(renderer.historySplices).toEqual([]);
    await act(async () => { mounted.unmount(); });
  });
});

describe("lazy scrollback", () => {
  /** One line per captured row, joined the way the host joins them. */
  function historyPage(rows: number): string {
    return Array.from({ length: rows }, (_, index) => `row-${index}`).join("\r\n");
  }

  /**
   * @param historySize what tmux says it is holding for this pane. Omitted is
   * the host's size probe going unanswered, which is a page to ask about again
   * rather than the end of the history.
   */
  function historyEvent(paneId: string, text: string, historySize?: number): PaneEvent {
    return {
      kind: "terminalHistory", paneId, sequence: 2,
      data: ownTerminalBytes(new TextEncoder().encode(text)),
      ...(historySize === undefined ? {} : { historySize }),
    };
  }

  it("asks for one page when a screen-seeded pane is scrolled to the top, and not again while the ask is outstanding", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h1"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h1", 4)); });

    await act(async () => { renderer.reachTop(); });
    // A page, not the whole history: 2,000 lines was ~195 KB in front of the
    // user's next keystroke.
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h1", 300, 0]]);

    // The answer has not arrived, so the second and third gestures are the same
    // question and cost nothing.
    await act(async () => { renderer.reachTop(); renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);

    await act(async () => { hub.deliver(historyEvent("%h1", "earlier output", 2_000)); });
    expect(renderer.historySplices).toEqual([{ bytes: 14, skip: 0, columns: 80, rows: 24 }]);
    await act(async () => { mounted.unmount(); });
  });

  it("says how much scrollback it already holds, so the answer starts above it", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h7"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h7", 4)); });
    // A seeded pane that has since printed: those rows scrolled off the screen
    // and are in this terminal's own scrollback. tmux measures its capture from
    // the *current* display, so without this number the answer would hand them
    // back a second time and the splice would show them twice.
    renderer.scrollbackRows = 37;

    await act(async () => { renderer.reachTop(); });

    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h7", 300, 37]]);
    await act(async () => { mounted.unmount(); });
  });

  /**
   * A resize is not a reseed, but it is just as fatal to a page in flight.
   *
   * Rewrapping moves rows across the boundary between what tmux keeps in its
   * history and what it shows on its display, so the skip the page quoted stops
   * naming where this buffer begins — and unlike ordinary output, the
   * difference is not rows this side gained, so the overlap the splice trims
   * cannot repair it. tmux resizes a pane whenever the window is split, closed
   * or dragged, so this is an ordinary thing to happen mid-page.
   */
  it("pages: drops the page in flight when tmux resizes the pane under it", async () => {
    const hub = new FakeHub();
    const pane = fixturePane("%h9");
    const mounted = await mountPane(pane, hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h9", 4)); });

    renderer.scrollbackRows = 40;
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h9", 300, 40]]);

    // tmux resizes the pane while the answer is still on the wire, and the same
    // rows rewrapped at a new width are a different number of rows.
    await updatePane(mounted, { ...pane, width: 100, height: 30 }, hub);
    expect(renderer.resizes).toEqual([{ columns: 100, rows: 30 }]);
    renderer.scrollbackRows = 33;

    await act(async () => { hub.deliver(historyEvent("%h9", historyPage(300), 2_000)); });
    expect(renderer.historySplices, "a page from before the resize was spliced").toEqual([]);

    // And the latch is open, so the gesture that reaches the top again asks
    // with the numbers that describe the buffer the user is looking at now.
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls.at(-1)).toEqual(["client-a", "%h9", 300, 33]);
    await act(async () => { mounted.unmount(); });
  });

  it("pages: a full answer leaves the next page to ask for, above the rows it just added", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h8"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h8", 4)); });

    await act(async () => { renderer.reachTop(); });
    // 2,000 lines above the screen and 300 asked for from the bottom of them:
    // there is more behind this page.
    await act(async () => { hub.deliver(historyEvent("%h8", historyPage(300), 2_000)); });
    // The splice put those rows in this buffer, so the next page starts above
    // them — the same rule as a pane that printed, and the reason no row is
    // ever fetched twice.
    renderer.scrollbackRows = 300;

    await act(async () => { renderer.reachTop(); });

    // And it asks for twice as much, because it is about to pay for rewriting
    // twice as much: every page replaces the whole buffer.
    expect(api.requestTerminalHistory.mock.calls).toEqual([
      ["client-a", "%h8", 300, 0],
      ["client-a", "%h8", 600, 300],
    ]);
    await act(async () => { mounted.unmount(); });
  });

  it("stops when the page it asked for reached the top of tmux's history", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h9"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h9", 4)); });

    await act(async () => { renderer.reachTop(); });
    // The rows asked for were 0 held + 300, and tmux holds 120: this page is
    // the whole of it. Decided on that number and never on the answer's own
    // rows, because tmux answers a range past the top with one clamped row
    // rather than with nothing.
    await act(async () => { hub.deliver(historyEvent("%h9", historyPage(120), 120)); });
    expect(renderer.historySplices).toHaveLength(1);
    renderer.scrollbackRows = 120;

    await act(async () => { renderer.reachTop(); renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("keeps paging when a page fills but leaves history behind it, counted from the rows it asked for", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h11"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h11", 4)); });
    // Already holding 500 rows, so this page asks for 501-800 of tmux's 900.
    renderer.scrollbackRows = 500;
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h11", 300, 500]]);

    // 500 + 300 < 900: a hundred rows are still above this.
    await act(async () => { hub.deliver(historyEvent("%h11", historyPage(300), 900)); });
    renderer.scrollbackRows = 800;
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls.at(-1)).toEqual(["client-a", "%h11", 600, 800]);

    // 800 + 600 >= 900: that was the last of it. Read from the page this
    // request actually asked for, which is no longer the first page's size.
    await act(async () => { hub.deliver(historyEvent("%h11", historyPage(100), 900)); });
    renderer.scrollbackRows = 900;
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(2);
    await act(async () => { mounted.unmount(); });
  });

  it("asks again when the host could not read how much history there is", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h12"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h12", 4)); });

    await act(async () => { renderer.reachTop(); });
    // The size probe is targeted and the pane went away under it. The rows
    // still arrived, so they are still spliced — but "tmux did not answer" is
    // not "there is nothing above this", and the question stays open.
    await act(async () => { hub.deliver(historyEvent("%h12", historyPage(300))); });
    expect(renderer.historySplices).toHaveLength(1);
    renderer.scrollbackRows = 300;

    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls.at(-1)).toEqual(["client-a", "%h12", 600, 300]);
    await act(async () => { mounted.unmount(); });
  });

  it("asks again after a splice the stream moved out from under", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h2"), hub);
    const renderer = renderers.created[0];
    renderer.historyOutcome = "superseded";
    await act(async () => { hub.deliver(seedEvent("%h2", 4)); });

    await act(async () => { renderer.reachTop(); });
    // A short page that could not be applied is not an answer about the history:
    // nothing was spliced, so nothing is latched either.
    await act(async () => { hub.deliver(historyEvent("%h2", historyPage(3), 3)); });
    expect(renderer.historySplices).toHaveLength(1);

    // Nothing was applied, so the question is still open and the next gesture
    // asks it again — against the screen the user is now looking at.
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(2);
    await act(async () => { mounted.unmount(); });
  });

  it("splices nothing for an answer with no rows, and still reads the size for whether to ask again", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%h3"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h3", 4)); });

    await act(async () => { renderer.reachTop(); });
    await act(async () => { hub.deliver(historyEvent("%h3", "", 0)); });

    // There is nothing above this screen, so nothing is spliced and nothing is
    // asked for again.
    expect(renderer.historySplices).toEqual([]);
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("fetches the first page as soon as the seeded pane has painted, and never before", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%p1"), hub);
    const renderer = renderers.created[0];

    await act(async () => { hub.deliver(seedEvent("%p1", 4)); });
    // The screen the user is waiting for goes first. Nothing is asked for while
    // the seed is still on its way to the glass.
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();

    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%p1", 300, 0]]);

    // Once, not once per paint.
    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("does not ask a second time when the user reaches the top during the prefetch", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%p2"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%p2", 4)); });
    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);

    await act(async () => { renderer.reachTop(); renderer.reachTop(); });

    // Same question, already outstanding.
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { mounted.unmount(); });
  });

  it("asks again for a pane the host reseeds while a page is still in flight", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%p5"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%p5", 4)); });
    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);

    // The answer to that page never comes — the hub drops this pane's events
    // while it owes a seed. A latch left standing would leave the new screen
    // unable to ask for its own first page for the life of this mount.
    await act(async () => { hub.deliver(seedEvent("%p5", 9)); });
    await act(async () => { renderer.flushRendered(); });

    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(2);
    await act(async () => { mounted.unmount(); });
  });

  it("asks for nothing on the alternate screen, where there is no scrollback to prepend to", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%p3"), hub);
    const renderer = renderers.created[0];
    renderer.alternateScreen = true;

    await act(async () => { hub.deliver(seedEvent("%p3", 4)); });
    await act(async () => { renderer.flushRendered(); });
    await act(async () => { renderer.reachTop(); });

    // The splice would be refused, so the page would have crossed the link to
    // be thrown away.
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();
    await act(async () => { mounted.unmount(); });
  });

  it("never prefetches for a pane that came up from its own cached screen", async () => {
    terminalStateCache.set(terminalCacheKey("local", "%p4"), "warm-screen", {
      checkpoint: { terminalEpoch: 7, outputGeneration: 3 },
      viewport: { atBottom: false, viewportLine: 4, grid: { columns: 80, rows: 24 } },
    });
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%p4"), hub);
    const renderer = renderers.created[0];
    expect(renderer.restoredSerialized).toBe("warm-screen");

    // That screen is a serialization of this pane's own buffer, scrollback
    // included: there is nothing above it the host is holding.
    await act(async () => { renderer.flushRendered(); });
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();
    await act(async () => { mounted.unmount(); });
  });

  it("still asks when the cached screen was itself a photograph", async () => {
    const hub = new FakeHub();
    const first = await mountPane(fixturePane("%h5"), hub);
    await act(async () => { hub.deliver(seedEvent("%h5", 4)); });
    await act(async () => { renderers.created[0].flushRendered(); });
    // The hide keeps that screen here and tells the host it did.
    await act(async () => { first.unmount(); });
    expect(terminalStateCache.get(terminalCacheKey("local", "%h5"))?.screenSeeded).toBe(true);
    expect(terminalStateCache.get(terminalCacheKey("local", "%h5"))?.historyExhausted).toBe(false);
    api.requestTerminalHistory.mockClear();

    const remounted = await mountPane(fixturePane("%h5"), hub);
    const renderer = renderers.created[1];
    expect(renderer.restoredSerialized).toBe("cached-screen");

    // Putting a photograph back on a fresh terminal does not give it a
    // scrollback: what is above this screen is still only in tmux. Restored
    // rather than seeded, so nothing is prefetched — the gesture asks.
    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();
    await act(async () => { renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(1);
    await act(async () => { remounted.unmount(); });
  });

  it("carries its paging across a hide: the restored screen continues above the pages it holds", async () => {
    const hub = new FakeHub();
    const first = await mountPane(fixturePane("%h6"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h6", 4)); });
    await act(async () => { renderer.flushRendered(); });
    await act(async () => { hub.deliver(historyEvent("%h6", historyPage(300), 2_000)); });
    renderer.scrollbackRows = 300;
    await act(async () => { first.unmount(); });

    // The page is in the buffer this screen was serialized from, and the entry
    // says so — the restore continues from there rather than fetching it again.
    expect(terminalStateCache.get(terminalCacheKey("local", "%h6"))?.screenSeeded).toBe(true);
    expect(terminalStateCache.get(terminalCacheKey("local", "%h6"))?.historyExhausted).toBe(false);
    // Including where the page ladder had got to: these bytes cost what they
    // cost to rewrite whichever mount is holding them.
    expect(terminalStateCache.get(terminalCacheKey("local", "%h6"))?.historyNextPageLines).toBe(600);
    api.requestTerminalHistory.mockClear();

    const remounted = await mountPane(fixturePane("%h6"), hub);
    const restored = renderers.created[1];
    restored.scrollbackRows = 300;
    await act(async () => { restored.reachTop(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%h6", 600, 300]]);
    await act(async () => { remounted.unmount(); });
  });

  it("carries the top of the history across a hide, so a restored pane stops asking too", async () => {
    const hub = new FakeHub();
    const first = await mountPane(fixturePane("%h10"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%h10", 4)); });
    await act(async () => { renderer.flushRendered(); });
    await act(async () => { hub.deliver(historyEvent("%h10", historyPage(12), 12)); });
    renderer.scrollbackRows = 12;
    await act(async () => { first.unmount(); });

    expect(terminalStateCache.get(terminalCacheKey("local", "%h10"))?.historyExhausted).toBe(true);
    api.requestTerminalHistory.mockClear();

    const remounted = await mountPane(fixturePane("%h10"), hub);
    const restored = renderers.created[1];
    restored.scrollbackRows = 12;
    await act(async () => { restored.flushRendered(); });
    await act(async () => { restored.reachTop(); });
    expect(api.requestTerminalHistory).not.toHaveBeenCalled();
    await act(async () => { remounted.unmount(); });
  });

  /**
   * A page in flight can outlive the screen it was asked against: a watchdog
   * reseed, or the host's own `emit_resnapshot` after it rejected a block,
   * replaces the screen while the answer is still on the wire — and the hub
   * delivers history outside its seed-debt ladder, so that answer still
   * arrives. It belongs to a buffer nothing is holding any more.
   */
  it("drops a page the reseed outran, and leaves the new screen's own page outstanding", async () => {
    const hub = new FakeHub();
    const mounted = await mountPane(fixturePane("%r1"), hub);
    const renderer = renderers.created[0];
    await act(async () => { hub.deliver(seedEvent("%r1", 4)); });
    await act(async () => { renderer.flushRendered(); });
    // Request A, asked against the screen the seed at generation 4 laid down.
    expect(api.requestTerminalHistory.mock.calls).toEqual([["client-a", "%r1", 300, 0]]);

    // The screen A was asked against is replaced, and the new one prefetches
    // its own first page: request B.
    await act(async () => { hub.deliver(seedEvent("%r1", 9)); });
    await act(async () => { renderer.flushRendered(); });
    expect(api.requestTerminalHistory.mock.calls).toEqual([
      ["client-a", "%r1", 300, 0],
      ["client-a", "%r1", 300, 0],
    ]);

    // A's answer, late. Its skip describes the screen that is gone; splicing it
    // now would put the user's earlier output above a screen it never sat
    // above, using a number B overwrote.
    await act(async () => { hub.deliver(historyEvent("%r1", historyPage(300), 2_000)); });
    expect(renderer.historySplices, "a superseded page was spliced above the new screen").toEqual([]);

    // And it is not B's answer either, so B's latch stands: reaching the top is
    // still the same outstanding question, not a third request that would fetch
    // rows B is about to deliver and show them twice.
    await act(async () => { renderer.reachTop(); renderer.reachTop(); });
    expect(api.requestTerminalHistory).toHaveBeenCalledTimes(2);

    // B's own answer, spliced above the screen B was asked against.
    await act(async () => { hub.deliver(historyEvent("%r1", historyPage(300), 2_000)); });
    expect(renderer.historySplices).toEqual([
      { bytes: historyPage(300).length, skip: 0, columns: 80, rows: 24 },
    ]);
    await act(async () => { mounted.unmount(); });
  });
});
