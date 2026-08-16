import { afterEach, describe, expect, it } from "vitest";
import {
  abandonPanePaintSpans,
  abandonPerfSpan,
  closePerfSpan,
  enablePerfProbe,
  openPerfSpan,
  perfSummary,
  resetPerfProbe,
} from "./probe";

describe("cross-component performance span ownership", () => {
  afterEach(() => resetPerfProbe());

  it("does not let a late old-scope failure abandon a replacement scope's span", () => {
    enablePerfProbe(async () => undefined);
    const oldScope = openPerfSpan("window.switch", "server-a");
    abandonPanePaintSpans("server-a");
    const replacementScope = openPerfSpan("window.switch", "server-b");

    abandonPerfSpan("window.switch", oldScope);
    closePerfSpan("window.switch", "server-b");

    expect(replacementScope).toBeDefined();
    expect(perfSummary()).toMatchObject([{ name: "window.switch", n: 1 }]);
  });

  it("keeps a shared span open when only one overlapping action fails", () => {
    enablePerfProbe(async () => undefined);
    const failed = openPerfSpan("window.switch");
    const accepted = openPerfSpan("window.switch");

    expect(failed).not.toBe(accepted);
    abandonPerfSpan("window.switch", failed);
    closePerfSpan("window.switch");

    expect(perfSummary()).toMatchObject([{ name: "window.switch", n: 1 }]);
  });

  it("does not let a late old-connection paint close the replacement span", () => {
    enablePerfProbe(async () => undefined);
    openPerfSpan("window.switch", "server-a");
    abandonPanePaintSpans("server-a");
    openPerfSpan("window.switch", "server-b");

    closePerfSpan("window.switch", "server-a");
    expect(perfSummary()).toEqual([]);
    closePerfSpan("window.switch", "server-b");
    expect(perfSummary()).toMatchObject([{ name: "window.switch", n: 1 }]);
  });
});
