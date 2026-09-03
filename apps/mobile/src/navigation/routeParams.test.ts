import { describe, expect, it } from "vitest";

import { fromRouteParam, toRouteParam } from "./routeParams";

const samples = [
  "%1",
  "%12",
  "%100",
  "%2512",
  "$3",
  "@7",
  "/home/x/100% done/file #1.txt",
  "a+b c?d=e&f",
  "naïve — 日本語 🚀",
  "~0025",
  "..",
  "",
];

describe("route param encoding", () => {
  it("round-trips tmux ids, paths and awkward strings exactly", () => {
    for (const sample of samples) {
      expect(fromRouteParam(toRouteParam(sample))).toBe(sample);
    }
  });

  it("emits only characters that every percent-decoding pass leaves alone", () => {
    for (const sample of samples) {
      const encoded = toRouteParam(sample);
      expect(encoded).toMatch(/^[A-Za-z0-9_~-]*$/);
      expect(encodeURIComponent(encoded)).toBe(encoded);
      expect(decodeURIComponent(encoded)).toBe(encoded);
      expect(new URLSearchParams(`v=${encoded}`).get("v")).toBe(encoded);
    }
  });

  it("is deterministic and readable for a pane id", () => {
    expect(toRouteParam("%12")).toBe("~002512");
    expect(toRouteParam("a-b_c")).toBe("a-b_c");
  });

  it("passes an undefined optional param through", () => {
    expect(fromRouteParam(undefined)).toBeUndefined();
  });

  it("leaves text the encoder never produces alone", () => {
    expect(fromRouteParam("plain")).toBe("plain");
    expect(fromRouteParam("~zz")).toBe("~zz");
  });
});
