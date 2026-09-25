import { describe, expect, it, vi } from "vitest";

import { startUpdateCheck, UPDATE_CHECK_INTERVAL_MS, type AvailableUpdate } from "./useUpdateCheck";

const update: AvailableUpdate = { version: "0.2.0", url: "https://github.com/gal064/muxflow/releases/tag/v0.2.0" };
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function harness(answers: Array<AvailableUpdate | null | Error>) {
  let tick: (() => void) | undefined;
  const deps = {
    check: vi.fn(async () => {
      const answer = answers.shift();
      if (answer instanceof Error) throw answer;
      return answer ?? null;
    }),
    setInterval: vi.fn((callback: () => void, ms: number) => { tick = callback; expect(ms).toBe(UPDATE_CHECK_INTERVAL_MS); return 7; }),
    clearInterval: vi.fn(),
  };
  return { deps, tick: () => tick?.() };
}

describe("startUpdateCheck", () => {
  it("checks at start and then daily, reporting each answer", async () => {
    const { deps, tick } = harness([null, update]);
    const results: Array<AvailableUpdate | null> = [];
    startUpdateCheck((value) => results.push(value), deps);
    await flush();
    expect(results).toEqual([null]);
    tick();
    await flush();
    expect(results).toEqual([null, update]);
    expect(deps.check).toHaveBeenCalledTimes(2);
  });

  it("keeps the last answer when a check fails", async () => {
    const { deps, tick } = harness([update, new Error("offline")]);
    const results: Array<AvailableUpdate | null> = [];
    startUpdateCheck((value) => results.push(value), deps);
    await flush();
    tick();
    await flush();
    expect(results).toEqual([update]);
  });

  it("reports nothing after it is stopped", async () => {
    const { deps } = harness([update]);
    const results: Array<AvailableUpdate | null> = [];
    const stop = startUpdateCheck((value) => results.push(value), deps);
    stop();
    await flush();
    expect(results).toEqual([]);
    expect(deps.clearInterval).toHaveBeenCalledWith(7);
  });
});
