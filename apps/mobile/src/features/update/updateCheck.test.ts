import { describe, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";

import { evaluateManifest, startUpdateCheck, UPDATE_CHECK_INTERVAL_MS, type UpdateState } from "./updateCheck";

const PAGE = "https://github.com/gal064/muxflow/releases/tag/v0.2.0";
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("evaluateManifest", () => {
  it("offers a newer release with its page", () => {
    expect(evaluateManifest("0.1.0", { version: "0.2.0", url: PAGE })).toEqual({ version: "0.2.0", url: PAGE });
    expect(evaluateManifest("0.1.9", { version: "0.1.10", url: PAGE })).not.toBeNull();
    expect(evaluateManifest("1.9.9", { version: "2.0.0", url: PAGE })).not.toBeNull();
  });

  it("offers nothing for the same or an older release", () => {
    expect(evaluateManifest("0.2.0", { version: "0.2.0", url: PAGE })).toBeNull();
    expect(evaluateManifest("0.3.0", { version: "0.2.0", url: PAGE })).toBeNull();
    expect(evaluateManifest("0.1.10", { version: "0.1.9", url: PAGE })).toBeNull();
  });

  it("refuses a link outside the Muxflow releases", () => {
    for (const url of [
      "https://example.com/muxflow",
      "http://github.com/gal064/muxflow/releases/tag/v0.2.0",
      "https://github.com/gal064/muxflow-evil/releases/tag/v0.2.0",
      42,
    ]) {
      expect(() => evaluateManifest("0.1.0", { version: "0.2.0", url })).toThrow();
    }
  });

  it("understands only a strict version", () => {
    for (const version of ["0.2", "0.2.0.1", "v0.2.0", "0.2.0-rc.1", "", 2]) {
      expect(() => evaluateManifest("0.1.0", { version, url: PAGE })).toThrow();
    }
    expect(() => evaluateManifest("0.1.0", null)).toThrow();
  });
});

describe("startUpdateCheck", () => {
  function harness(answers: unknown[]) {
    let clock = 1_000;
    let foreground: (() => void) | undefined;
    const store = createStore<UpdateState>(() => ({ update: null }));
    const deps = {
      runningVersion: "0.1.0",
      fetchManifest: vi.fn(async () => {
        const answer = answers.shift();
        if (answer instanceof Error) throw answer;
        return answer;
      }),
      now: () => clock,
      onForeground: (callback: () => void) => { foreground = callback; },
      log: vi.fn(),
    };
    return { deps, store, advance: (ms: number) => { clock += ms; }, foreground: () => foreground?.() };
  }

  it("checks at launch and again on a foreground a day later", async () => {
    const { deps, store, advance, foreground } = harness([{ version: "0.1.0", url: PAGE }, { version: "0.2.0", url: PAGE }]);
    startUpdateCheck(deps, store);
    await flush();
    expect(store.getState().update).toBeNull();
    advance(60_000);
    foreground();
    expect(deps.fetchManifest).toHaveBeenCalledTimes(1);
    advance(UPDATE_CHECK_INTERVAL_MS);
    foreground();
    await flush();
    expect(deps.fetchManifest).toHaveBeenCalledTimes(2);
    expect(store.getState().update).toEqual({ version: "0.2.0", url: PAGE });
  });

  it("keeps the last answer when a check fails or the manifest is untrustworthy", async () => {
    const { deps, store, advance, foreground } = harness([
      { version: "0.2.0", url: PAGE },
      new Error("offline"),
      { version: "0.3.0", url: "https://example.com/" },
    ]);
    startUpdateCheck(deps, store);
    await flush();
    for (let attempt = 0; attempt < 2; attempt += 1) {
      advance(UPDATE_CHECK_INTERVAL_MS);
      foreground();
      await flush();
    }
    expect(store.getState().update).toEqual({ version: "0.2.0", url: PAGE });
    expect(deps.log).toHaveBeenCalledTimes(2);
  });
});
