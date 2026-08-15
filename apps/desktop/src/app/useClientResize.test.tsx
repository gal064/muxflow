// @vitest-environment jsdom
import { useEffect } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PixelBox, TerminalMeasurements } from "../features/terminal/TerminalRenderer";
import { CLIENT_RESIZE_DEBOUNCE_MS, CLIENT_RESIZE_RETRIES, CLIENT_RESIZE_RETRY_MS, useClientResize } from "./useClientResize";

const resizeClientMock = vi.hoisted(() => vi.fn(async (_clientId: string, _columns: number, _rows: number) => undefined));
vi.mock("../features/terminal/api", () => ({ resizeClient: resizeClientMock }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const MEASUREMENTS: TerminalMeasurements = {
  cell: { width: 8, height: 17 },
  chrome: { horizontal: 12, vertical: 12, scrollbar: 14 },
};

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
  activeSessionId?: string;
  canMutate?: boolean;
  clientId?: string;
  measurements?: TerminalMeasurements;
  onStatus?: (message: string) => void;
  surfaceMounted?: boolean;
}

function Harness(props: HarnessProps) {
  const { onMeasurements, surfaceRef } = useClientResize({
    activeWindowId: props.activeWindowId,
    activeSessionId: props.activeSessionId,
    canMutate: props.canMutate ?? true,
    clientId: props.clientId,
    onStatus: props.onStatus ?? (() => undefined),
  });
  const measurements = "measurements" in props ? props.measurements : MEASUREMENTS;
  useEffect(() => { if (measurements) onMeasurements(measurements); }, [measurements, onMeasurements]);
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

  it("waits for a terminal to report what it turns pixels into", async () => {
    const statuses: string[] = [];
    const { update } = await render({
      clientId: "client-1",
      measurements: undefined,
      onStatus: (message) => statuses.push(message),
    });
    // Nothing yet, and nothing said: a terminal is expected to arrive.
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toEqual([]);
    // It arrives; the value itself is the trigger.
    await update({ measurements: MEASUREMENTS });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46]]);
  });

  it("re-asks when the cell size changes under it, with no remount", async () => {
    const { update } = await render({ clientId: "client-1" });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46]]);
    // What a move between displays of different pixel ratios does.
    await update({ measurements: { ...MEASUREMENTS, cell: { width: 10, height: 20 } } });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-1", 97, 39]]);
  });

  it("stays quiet when the window is merely too small for a terminal", async () => {
    const statuses: string[] = [];
    await render({ clientId: "client-1", onStatus: (message) => statuses.push(message) }, { width: 40, height: 800 });
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
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    // A new bridge has a new tmux client, which has never been sized.
    await update({ clientId: "client-2" });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-2", 121, 46]]);
  });

  // M13-E005: the user selected a workspace on another tmux session and its
  // windows collapsed to 80x24. The host attaches one control client per
  // session and only the visible one participates in sizing, so selecting a
  // session hands sizing to a client that has never been given a size — while
  // the bridge's `clientId` and the app's surface both stayed exactly as they
  // were, so the dedupe answered "already sent" for a client that had never
  // been sent anything.
  it("sizes the newly selected session's client, which has never been sized", async () => {
    const { update } = await render({ clientId: "client-1", activeSessionId: "$1", activeWindowId: "@1" });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    await update({ activeSessionId: "$2" });
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-1", 121, 46]]);
    // Still once per client, though: selecting the same session again is not a
    // new client, and a repeated `refresh-client -C` is the churn this dedupes.
    await update({ activeSessionId: "$2" });
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
    // And a new session created and selected on the same bridge is another one.
    await update({ activeSessionId: "$3" });
    expect(resizeClientMock).toHaveBeenCalledTimes(3);
  });

  it("retries a request the bridge refused, then reports it", async () => {
    resizeClientMock.mockImplementation(async () => { throw new Error("bridge disconnected"); });
    const statuses: string[] = [];
    await render({ clientId: "client-1", onStatus: (message) => statuses.push(message) });
    // Nothing else would ever ask again: the triggers are a window change, a
    // surface change and a reconnect, and a desktop nobody resizes has none.
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    expect(statuses).toEqual([]);
    await exhaustRetries();
    expect(resizeClientMock).toHaveBeenCalledTimes(CLIENT_RESIZE_RETRIES + 1);
    expect(resizeClientMock.mock.calls.every(([, columns, rows]) => columns === 121 && rows === 46)).toBe(true);
    expect(statuses).toEqual(["Error: bridge disconnected"]);
  });

  it("stops retrying as soon as one lands", async () => {
    resizeClientMock.mockImplementationOnce(async () => { throw new Error("bridge disconnected"); });
    await render({ clientId: "client-1" });
    await exhaustRetries();
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
  });

  it("refuses an out-of-bound size, says so, and sends nothing", async () => {
    const statuses: string[] = [];
    const { update } = await render(
      {
        clientId: "client-1",
        measurements: { cell: { width: 1, height: 1 }, chrome: MEASUREMENTS.chrome },
        onStatus: (message) => statuses.push(message),
      },
      { width: 4000, height: 4000 },
    );
    expect(resizeClientMock).not.toHaveBeenCalled();
    expect(statuses).toHaveLength(1);
    expect(statuses[0]).toContain("3974x3988");
    // Said once, not once per recompute.
    await update({ activeWindowId: "@2" });
    expect(statuses).toHaveLength(1);
  });
});
