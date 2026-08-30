/**
 * Whether this pane is following live output or showing history the user is
 * actively reading.
 *
 * `outdated` means live bytes arrived after the user left the bottom. Those
 * bytes deliberately did not touch the displayed xterm. `refreshing` is the
 * single authoritative seed requested when the user returns live.
 */
export type TerminalReadingState = "live" | "reading" | "outdated" | "refreshing";

export interface ReturnToLiveDecision {
  state: TerminalReadingState;
  requestSeed: boolean;
  scrollLocally: boolean;
}

export function readingStateForViewport(
  state: TerminalReadingState,
  atBottom: boolean,
): TerminalReadingState {
  if (state === "refreshing") return state;
  if (!atBottom && state === "live") return "reading";
  return state;
}

/** Output may paint only while the pane is following the live bottom. */
export function readingStateForOutput(
  state: TerminalReadingState,
): { state: TerminalReadingState; render: boolean; becameOutdated: boolean } {
  if (state === "reading") return { state: "outdated", render: false, becameOutdated: true };
  if (state === "outdated") return { state, render: false, becameOutdated: false };
  return { state, render: state === "live", becameOutdated: false };
}

/** Returns to live output, refreshing only when bytes were deliberately skipped. */
export function returnToLive(state: TerminalReadingState): ReturnToLiveDecision {
  if (state === "outdated") {
    return { state: "refreshing", requestSeed: true, scrollLocally: false };
  }
  if (state === "reading") {
    return { state: "live", requestSeed: false, scrollLocally: true };
  }
  return { state, requestSeed: false, scrollLocally: false };
}

export function readingStateForAuthoritativeScreen(): TerminalReadingState {
  return "live";
}

/** Only live and still-current reading screens may become resumable snapshots. */
export function readingScreenIsCurrent(state: TerminalReadingState): boolean {
  return state === "live" || state === "reading";
}

export function readingMayPageHistory(state: TerminalReadingState): boolean {
  return state === "live" || state === "reading";
}
