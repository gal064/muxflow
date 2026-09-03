// The talk pane's large mode (design.md §9.11), kept as plain arithmetic so it is testable without the screen.

/** The talk pane in its large mode takes this much of the height under the header; the list keeps the rest (§9.11). */
export const BIG_PANE_FRACTION = 0.7;
/** …but never less than this for the list: a collapsed reply with its player, so the newest turn stays readable. */
export const MIN_LIST_HEIGHT = 168;

/** The large pane's height: 70 % of what is left under the header, capped so the list keeps `MIN_LIST_HEIGHT`. */
export function bigPaneHeight(windowHeight: number, insetTop: number, insetBottom: number, headerHeight: number): number {
  const available = Math.max(0, windowHeight - insetTop - insetBottom - headerHeight);
  return Math.max(0, Math.min(Math.round(available * BIG_PANE_FRACTION), available - MIN_LIST_HEIGHT));
}
