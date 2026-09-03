// The working spinner's clock and proportions (design.md §9.3.1), as data so
// a test can hold the numbers still.

/** One revolution: the desktop `.spinner`'s `.9s linear infinite`. */
export const SPINNER_TURN_MS = 900;

/**
 * How much of the ring the moving arc covers. The desktop's `.spinner` paints
 * a quarter (one border side); at 11 dp a quarter in the same ink as the track
 * froze into a plain ring, so the arc is a bit under a third and heavier than
 * the track, and a still frame still reads as "in progress".
 */
export const SPINNER_ARC_TURNS = 0.3;

/** Track opacity, the desktop's 35 %: in the heading's `--chrome-dim` a fainter track vanished on `--chrome-bg`. */
export const SPINNER_TRACK_OPACITY = 0.35;
