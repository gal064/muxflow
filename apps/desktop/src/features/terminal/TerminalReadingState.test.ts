import { describe, expect, it } from "vitest";
import {
  readingMayPageHistory,
  readingScreenIsCurrent,
  readingStateForAuthoritativeScreen,
  readingStateForOutput,
  readingStateForViewport,
  returnToLive,
  type TerminalReadingState,
} from "./TerminalReadingState";

describe("terminal reading state", () => {
  it("starts reading when the viewport leaves the live bottom", () => {
    expect(readingStateForViewport("live", false)).toBe("reading");
    expect(readingStateForViewport("live", true)).toBe("live");
  });

  it("keeps the first and later live bytes out of the reading screen", () => {
    expect(readingStateForOutput("reading")).toEqual({
      state: "outdated",
      render: false,
      becameOutdated: true,
    });
    expect(readingStateForOutput("outdated")).toEqual({
      state: "outdated",
      render: false,
      becameOutdated: false,
    });
    expect(readingStateForOutput("live")).toEqual({
      state: "live",
      render: true,
      becameOutdated: false,
    });
  });

  it("returns immediately when nothing changed and requests one seed otherwise", () => {
    expect(returnToLive("reading")).toEqual({
      state: "live",
      requestSeed: false,
      scrollLocally: true,
    });
    expect(returnToLive("outdated")).toEqual({
      state: "refreshing",
      requestSeed: true,
      scrollLocally: false,
    });
    expect(returnToLive("refreshing")).toEqual({
      state: "refreshing",
      requestSeed: false,
      scrollLocally: false,
    });
  });

  it("accepts an authoritative screen from every state", () => {
    for (const state of ["live", "reading", "outdated", "refreshing"] satisfies TerminalReadingState[]) {
      expect(readingStateForAuthoritativeScreen()).toBe("live");
      expect(readingStateForViewport(state, false)).toBe(state === "live" ? "reading" : state);
    }
  });

  it("never caches or extends history after the reading screen became stale", () => {
    for (const state of ["outdated", "refreshing"] satisfies TerminalReadingState[]) {
      expect(readingScreenIsCurrent(state)).toBe(false);
      expect(readingMayPageHistory(state)).toBe(false);
    }
    for (const state of ["live", "reading"] satisfies TerminalReadingState[]) {
      expect(readingScreenIsCurrent(state)).toBe(true);
      expect(readingMayPageHistory(state)).toBe(true);
    }
  });
});
