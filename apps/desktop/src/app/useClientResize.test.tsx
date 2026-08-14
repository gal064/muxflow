// @vitest-environment jsdom
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cellsForBox, type PixelBox, type TerminalSize } from "../features/terminal/TerminalRenderer";
import { CLIENT_RESIZE_DEBOUNCE_MS, CLIENT_RESIZE_RETRIES, CLIENT_RESIZE_RETRY_MS, useClientResize } from "./useClientResize";

const resizeClientMock = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock("../features/terminal/api", () => ({ resizeClient: resizeClientMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const CELL: PixelBox = { width: 8, height: 17 };
const CHROME = { horizontal: 12, vertical: 12, scrollbar: 14 };
const measureBox = (box: PixelBox): TerminalSize | undefined => cellsForBox(box, CELL, CHROME);

/**
 * Every surface element the harness mounts, with a settable pixel box. jsdom
 * lays nothing out, so the box and the observer are supplied here.
 */
const observed = new Set<{ element: HTMLElement; notify: () => void }>();

class StubResizeObserver {
  #entry?: { element: HTMLElement; notify: () => void };
  constructor(private readonly callback: () => void) {}
  observe(element: HTMLElement) {
    this.#entry = { element, notify: () => this.callback() };
    observed.add(this.#entry);
  }
  disconnect() {
    if (this.#entry) observed.delete(this.#entry);
    this.#entry = undefined;
  }
}

function setSurfaceBox(box: PixelBox) {
  for (const { element, notify } of observed) {
    element.getBoundingClientRect = () => ({ ...box, top: 0, left: 0, right: box.width, bottom: box.height, x: 0, y: 0, toJSON: () => "" });
    notify();
  }
}

interface HarnessProps {
  activeWindowId?: string;
  canMutate?: boolean;
  clientId?: string;
  measure?: (box: PixelBox) => TerminalSize | undefined;
  metricsKey?: string;
  onStatus?: (message: string) => void;
  surfaceMounted?: boolean;
}

function Harness(props: HarnessProps) {
  const { surfaceRef } = useClientResize({
    activeWindowId: props.activeWindowId,
    canMutate: props.canMutate ?? true,
    clientId: props.clientId,
    measureBox: props.measure ?? measureBox,
    metricsKey: props.metricsKey,
    onStatus: props.onStatus ?? (() => undefined),
  });
  return props.surfaceMounted === false ? null : <div ref={surfaceRef} />;
}

async function render(props: HarnessProps, box: PixelBox = { width: 1000, height: 800 }) {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness {...props} />, { createNodeMock: () => document.createElement("div") }); });
  await act(async () => { setSurfaceBox(box); });
  await settle();
  return {
    renderer,
    update: async (next: HarnessProps) => {
      await act(async () => { renderer.update(<Harness {...props} {...next} />); });
      await settle();
    },
  };
}

async function settle() {
  await act(async () => { await vi.advanceTimersByTimeAsync(CLIENT_RESIZE_DEBOUNCE_MS + 1); });
}

/** Long enough for every retry the hook is allowed. */
async function exhaustRetries() {
  await act(async () => { await vi.advanceTimersByTimeAsync(CLIENT_RESIZE_RETRY_MS * (CLIENT_RESIZE_RETRIES + 1)); });
}

describe("useClientResize", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resizeClientMock.mockClear();
    resizeClientMock.mockImplementation(async () => undefined);
    observed.clear();
    vi.stubGlobal("ResizeObserver", StubResizeObserver);
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("asks for the surface's own size once the connection is live", async () => {
    await render({ clientId: "client-1" });
    // 1000 − 2 frame − 12 padding − 14 scrollbar = 972 → 121 columns;
    // 800 − 2 − 12 = 786 → 46 rows.
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46]]);
  });

  it("asks for nothing before a client, before mutation is allowed, or with no surface", async () => {
    await render({ clientId: undefined });
    await render({ clientId: "client-1", canMutate: false });
    await render({ clientId: "client-1", surfaceMounted: false });
    expect(resizeClientMock).not.toHaveBeenCalled();
  });

  it("waits for a terminal to report metrics, then says so rather than staying silent", async () => {
    const statuses: string[] = [];
    let metrics: ((box: PixelBox) => TerminalSize | undefined) = () => undefined;
    const { update } = await render({
      clientId: "client-1",
      measure: (box) => metrics(box),
      onStatus: (message) => statuses.push(message),
    });
    // Nothing yet, and nothing said: a renderer is expected to arrive.
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toEqual([]);
    metrics = measureBox;
    await act(async () => { await vi.advanceTimersByTimeAsync(CLIENT_RESIZE_RETRY_MS + 1); });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46]]);

    // A connection whose terminals never report metrics is a broken app that
    // must not look like a working one.
    metrics = () => undefined;
    resizeClientMock.mockClear();
    await update({ activeWindowId: "@2" });
    await exhaustRetries();
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toHaveLength(1);
    expect(statuses[0]).toContain("could not be computed");
  });

  it("stays quiet when the window is merely too small for a terminal", async () => {
    const statuses: string[] = [];
    await render({ clientId: "client-1", onStatus: (message) => statuses.push(message) }, { width: 40, height: 800 });
    await exhaustRetries();
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toEqual([]);
  });

  it("follows the surface and coalesces a drag into one request", async () => {
    const { renderer } = await render({ clientId: "client-1" });
    resizeClientMock.mockClear();
    await act(async () => {
      setSurfaceBox({ width: 900, height: 800 });
      setSurfaceBox({ width: 800, height: 800 });
      setSurfaceBox({ width: 700, height: 800 });
      await vi.advanceTimersByTimeAsync(CLIENT_RESIZE_DEBOUNCE_MS - 5);
    });
    expect(resizeClientMock).not.toHaveBeenCalled();
    await settle();
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 84, 46]]);
    await act(async () => renderer.unmount());
  });

  it("recomputes for a new tmux window and a new connection, and repeats nothing", async () => {
    const { update } = await render({ clientId: "client-1", activeWindowId: "@1" });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    // Same surface, different window: the answer is unchanged, and a repeated
    // `refresh-client -C` would only re-assert this client's size over the
    // plain terminals sharing the session.
    await update({ activeWindowId: "@2" });
    await update({ metricsKey: "%7" });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    // A new bridge has a new tmux client, which has never been sized.
    await update({ clientId: "client-2" });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-2", 121, 46]]);
  });

  it("retries the size a failed request never delivered", async () => {
    resizeClientMock.mockImplementationOnce(async () => { throw new Error("bridge disconnected"); });
    const statuses: string[] = [];
    const { update } = await render({ clientId: "client-1", onStatus: (message) => statuses.push(message) });
    expect(statuses).toEqual(["Error: bridge disconnected"]);
    await update({ activeWindowId: "@2" });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-1", 121, 46]]);
  });

  it("refuses an out-of-bound size, says so, and sends nothing", async () => {
    const statuses: string[] = [];
    await render(
      { clientId: "client-1", measure: (box) => cellsForBox(box, { width: 1, height: 1 }, CHROME), onStatus: (message) => statuses.push(message) },
      { width: 4000, height: 4000 },
    );
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toHaveLength(1);
    expect(statuses[0]).toContain("3972x3986");
  });
});
