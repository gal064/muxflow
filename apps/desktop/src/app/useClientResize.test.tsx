// @vitest-environment jsdom
import { useEffect } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PixelBox, TerminalMeasurements, TerminalSize } from "../features/terminal/TerminalRenderer";
import { CLIENT_RESIZE_DEBOUNCE_MS, CLIENT_RESIZE_RETRIES, CLIENT_RESIZE_RETRY_MS, CLIENT_RESIZE_TAKE_INTERVAL_MS, useClientResize } from "./useClientResize";

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
  actualSize?: TerminalSize;
  canMutate?: boolean;
  clientId?: string;
  measurements?: TerminalMeasurements;
  onStatus?: (message: string) => void;
  surfaceMounted?: boolean;
}

function Harness(props: HarnessProps) {
  const { onMeasurements, surfaceRef } = useClientResize({
    activeWindowId: props.activeWindowId,
    actualSize: props.actualSize,
    canMutate: props.canMutate ?? true,
    clientId: props.clientId,
    onStatus: props.onStatus ?? (() => undefined),
  });
  const measurements = "measurements" in props ? props.measurements : MEASUREMENTS;
  useEffect(() => { if (measurements) onMeasurements(measurements); }, [measurements, onMeasurements]);
  return props.surfaceMounted === false ? null : <div ref={surfaceRef} />;
}

/** Every hook the file mounts listens on the one jsdom `window`; each test unmounts its own. */
const mounted: ReactTestRenderer[] = [];

async function render(props: HarnessProps, box: PixelBox = { width: 1000, height: 800 }) {
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<Harness {...props} />, { createNodeMock: () => document.createElement("div") }); });
  mounted.push(renderer);
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

/** What a person does in the app: a keystroke, then the debounce. */
async function keydown() {
  await act(async () => { window.dispatchEvent(new KeyboardEvent("keydown", { key: "a" })); });
  await settle();
}

async function pointerdown() {
  await act(async () => { window.dispatchEvent(new Event("pointerdown")); });
  await settle();
}

async function idle(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

describe("useClientResize", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resizeClientMock.mockClear();
    resizeClientMock.mockImplementation(async () => undefined);
    observed.clear();
    vi.stubGlobal("ResizeObserver", StubResizeObserver);
  });
  afterEach(async () => {
    for (const renderer of mounted.splice(0)) await act(async () => renderer.unmount());
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


  /**
   * Under `window-size latest`, another client on the same session — a plain
   * terminal, or the phone — takes the windows to its own size the moment it
   * is used. The app's surface has not moved, so the ordinary path recomputes
   * the same answer and the dedupe drops it — which is how a pane stays
   * letterboxed for the rest of the session. A keystroke here is what takes
   * it back.
   */
  it("takes the size back on a keystroke when tmux's windows are not the size that was asked for", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    // The phone shrank the shared windows; nothing happens until the user does something.
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-1", 121, 46]]);
  });

  it("takes on a pointer-down as well", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await pointerdown();
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
  });

  /**
   * Two clients that each answer the other's size resize a real person's
   * windows back and forth for as long as both are used, so a take is allowed
   * once per interval and no more — the flicker while two people type at once
   * is slow, and stops the moment one of them does.
   */
  it("takes at most once per interval, however much is typed", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
    // The other side answers with its own size again; typing inside the
    // interval changes nothing.
    await update({ ...props, actualSize: { columns: 80, rows: 25 } });
    await keydown();
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
    // Past the interval, the next keystroke takes again.
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(3);
  });

  /**
   * The whole anti-resize-war policy: an app nobody is using never takes,
   * however often the other side moves the windows. Focus is not use — an
   * app left open beside a phone in use must stay quiet.
   */
  it("never takes without an interaction, however many times tmux's answer changes", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    for (let round = 0; round < 4; round += 1) {
      await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
      await update({ ...props, actualSize: { columns: 80, rows: 24 + round } });
    }
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
  });

  it("takes nothing while tmux has the size that was asked for", async () => {
    const props = { clientId: "client-1", actualSize: { columns: 121, rows: 46 } };
    await render(props);
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    await pointerdown();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
  });

  /** A window the snapshot cannot describe is not a window at 0x0. */
  it("takes nothing when the snapshot reports no size at all", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    await update({ ...props, actualSize: undefined });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
  });

  /**
   * The ordinary path is the same rate limit's other half: the first request
   * after a connection is a send too, so a keystroke right behind it — while
   * the snapshot still shows the size from before — does not double it.
   */
  it("counts the ordinary request against the interval", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 80, rows: 24 } });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    await update({ ...props, actualSize: { columns: 121, rows: 46 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
  });

  /**
   * The surface moved a few milliseconds before a keystroke: the ordinary
   * request goes out in the debounce, and the take that was due at the
   * keystroke must notice it went and not repeat it.
   */
  it("does not repeat an ordinary request that went out while the take was pending", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await act(async () => {
      setSurfaceBox({ width: 900, height: 800 });
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));
    });
    await settle();
    await settle();
    expect(resizeClientMock.mock.calls).toEqual([["client-1", 121, 46], ["client-1", 109, 46]]);
  });

  /**
   * The take is now the only route that hands the width back, so a bridge
   * that refuses one must not switch takes off for the rest of the connection.
   */
  it("takes again after a take the bridge refused", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    resizeClientMock.mockImplementation(async () => { throw new Error("visible session control client is detached"); });
    await keydown();
    await exhaustRetries();
    expect(resizeClientMock).toHaveBeenCalledTimes(2 + CLIENT_RESIZE_RETRIES);
    resizeClientMock.mockImplementation(async () => undefined);
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(3 + CLIENT_RESIZE_RETRIES);
    expect(resizeClientMock.mock.lastCall).toEqual(["client-1", 121, 46]);
  });

  /**
   * With an app tab showing there is no tiled surface to measure, so nothing
   * may be sent — and the app must still be able to take its size back when
   * the terminal comes back.
   */
  it("sends nothing while no terminal surface is mounted, and recovers after", async () => {
    const props = { clientId: "client-1" };
    const { update } = await render({ ...props, actualSize: { columns: 121, rows: 46 } });
    expect(resizeClientMock).toHaveBeenCalledTimes(1);

    // The user opens a file: the surface unmounts, and tmux is meanwhile taken
    // to someone else's size. Typing into the file is not a take.
    await update({ ...props, surfaceMounted: false, actualSize: { columns: 80, rows: 24 } });
    await idle(CLIENT_RESIZE_TAKE_INTERVAL_MS);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);

    // Back to the terminal. The remounted surface needs its box again — jsdom
    // lays nothing out. Its size is the one already asked for, so the ordinary
    // path repeats nothing; the next keystroke is what takes the size back.
    await update({ ...props, actualSize: { columns: 80, rows: 24 } });
    await act(async () => { setSurfaceBox({ width: 1000, height: 800 }); });
    await settle();
    expect(resizeClientMock).toHaveBeenCalledTimes(1);
    await keydown();
    expect(resizeClientMock).toHaveBeenCalledTimes(2);
  });
});
