// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FONT_READY_TIMEOUT_MS, waitForTerminalFonts } from "./fontGate";

const faces = vi.hoisted(() => ({ settle: () => undefined as void, promise: Promise.resolve([]) as Promise<unknown> }));
vi.mock("../features/terminal/theme", () => ({ terminalFacesReady: () => faces.promise }));

/**
 * What the pre-mount font wait costs, measured rather than assumed.
 *
 * The gate is deliberately kept — a terminal built before JetBrains Mono is
 * usable measures the fallback cell, and the tmux grid is computed from that
 * cell — so what matters is that its cost is bounded and that nothing can make
 * the app fail to mount. Both numbers are recorded here: a face that is already
 * usable costs one turn of the loop, and a face that never arrives costs
 * exactly the 2 s bound.
 */
describe("the terminal font gate", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    Object.defineProperty(document, "fonts", {
      configurable: true,
      value: { ready: Promise.resolve() },
    });
  });
  afterEach(() => {
    vi.useRealTimers();
    Reflect.deleteProperty(document, "fonts");
  });

  it("costs nothing beyond a microtask once the terminal faces are usable", async () => {
    faces.promise = Promise.resolve([]);
    const started = performance.now();
    const outcome = await waitForTerminalFonts();
    expect(outcome).toBe("ready");
    expect(performance.now() - started, "the ready path waited on a timer").toBeLessThan(1);
  });

  it("gives up after exactly the bound when the faces never arrive", async () => {
    faces.promise = new Promise(() => undefined);
    const wait = waitForTerminalFonts();
    await vi.advanceTimersByTimeAsync(FONT_READY_TIMEOUT_MS - 1);
    let settled = false;
    void wait.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled, "the gate gave up before its own bound").toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await expect(wait).resolves.toBe("timeout");
  });

  it("mounts anyway when the face wait fails outright", async () => {
    faces.promise = Promise.reject(new Error("no font subsystem"));
    await expect(waitForTerminalFonts()).resolves.toBe("unavailable");
  });

  it("does not wait at all where the document has no font access", async () => {
    Reflect.deleteProperty(document, "fonts");
    await expect(waitForTerminalFonts()).resolves.toBe("unavailable");
  });
});
