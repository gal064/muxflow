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
  class FakeRenderer {
    writes: string[] = [];
    disposed = false;
    restoredSerialized: string | undefined;
    #pendingRendered: Array<() => void> = [];

    open(): void {}
    measure(): undefined { return undefined; }
    measurements(): undefined { return undefined; }
    setGrid(): { kind: "unchanged" } { return { kind: "unchanged" }; }
    onInput(): () => void { return () => undefined; }
    onViewportChange(): () => void { return () => undefined; }
    focus(): void {}
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
  return { FakeRenderer, renderers: { created: [] as InstanceType<typeof FakeRenderer>[] } };
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

async function mountPane(pane: Pane, hub: FakeHub, clientId = "client-a"): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<TerminalPane
      clientId={clientId}
      pane={pane}
      hub={hub.asHub()}
      onInput={() => undefined}
      onFocus={() => undefined}
      onMeasurements={() => undefined}
      onController={() => undefined}
    />, { createNodeMock: () => ({ addEventListener: () => undefined, removeEventListener: () => undefined }) });
  });
  return renderer;
}

describe("TerminalPane pane-paint span lifecycle", () => {
  beforeEach(() => {
    resetPerfProbe();
    enablePerfProbe(async () => undefined);
    terminalStateCache.clear();
    renderers.created.length = 0;
    api.setTerminalVisibility.mockClear();
    api.requestTerminalSeed.mockClear();
    Object.assign(globalThis, {
      IS_REACT_ACT_ENVIRONMENT: true,
      ResizeObserver: class { observe() {} unobserve() {} disconnect() {} },
    });
  });
  afterEach(() => {
    resetPerfProbe();
    terminalStateCache.clear();
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
    await act(async () => { renderers.created[0].flushRendered(); });
    await awaitPaint();

    expect(perfSummary().map(({ name }) => name)).toContain("window.switch");
    await act(async () => renderer.unmount());
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
