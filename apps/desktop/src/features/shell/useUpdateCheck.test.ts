import { describe, expect, it, vi } from "vitest";

import { installUpdate, startUpdateCheck, UPDATE_CHECK_INTERVAL_MS, type AvailableUpdate } from "./useUpdateCheck";

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

describe("installUpdate", () => {
  function installDeps(confirmed = true) {
    return {
      confirm: vi.fn(async () => confirmed),
      install: vi.fn(async () => undefined),
      openReleasePage: vi.fn(async () => undefined),
      setStatus: vi.fn(),
    };
  }

  it("installs once the user agrees to the restart", async () => {
    const deps = installDeps();
    await installUpdate(update, deps);
    expect(deps.install).toHaveBeenCalledTimes(1);
    expect(deps.openReleasePage).not.toHaveBeenCalled();
  });

  it("leaves everything alone when the user declines", async () => {
    const deps = installDeps(false);
    await installUpdate(update, deps);
    expect(deps.install).not.toHaveBeenCalled();
    expect(deps.openReleasePage).not.toHaveBeenCalled();
  });

  it("reports a failed install and opens the release page", async () => {
    const deps = installDeps();
    deps.install.mockRejectedValueOnce("update failed: offline");
    await installUpdate(update, deps);
    expect(deps.setStatus).toHaveBeenLastCalledWith("update failed: offline");
    expect(deps.openReleasePage).toHaveBeenCalledWith(update.url);
  });

  it("ignores a second click while one is running", async () => {
    const deps = installDeps();
    const first = installUpdate(update, deps);
    await installUpdate(update, deps);
    await first;
    expect(deps.confirm).toHaveBeenCalledTimes(1);
  });
});

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
