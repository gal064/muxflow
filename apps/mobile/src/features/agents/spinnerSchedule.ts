// The working spinner's clock and proportions (design.md §9.3.1), as data so
// the cost is a number a test can hold still and the QA agent can measure.

/**
 * One turn is drawn as this many discrete frames rather than a continuous
 * rotation. A continuous spin repaints the whole tab every vsync for as long
 * as one agent is working; twelve 30° steps read as motion at reading
 * distance and cost ten repaints a second instead of sixty.
 */
export const SPINNER_STEPS = 12;

/** How long each frame holds. */
export const SPINNER_STEP_MS = 100;

/** One revolution. The desktop turns in 0.9 s; the steps want a round 100 ms. */
export const SPINNER_TURN_MS = SPINNER_STEPS * SPINNER_STEP_MS;

/** Degrees per frame. */
export const SPINNER_STEP_DEGREES = 360 / SPINNER_STEPS;

/**
 * How much of the ring the moving arc covers. The desktop's `.spinner` paints
 * a quarter (one border side); at 11 dp a quarter in the same ink as the track
 * froze into a plain ring, so the arc is a bit under a third and heavier than
 * the track, and a still frame still reads as "in progress".
 */
export const SPINNER_ARC_TURNS = 0.3;

/** Track opacity. The desktop's is 35 %; lower here so the arc stands off it. */
export const SPINNER_TRACK_OPACITY = 0.25;

/** The upper bound on repaints per second one visible spinner causes. */
export const SPINNER_REPAINTS_PER_SECOND = 1000 / SPINNER_STEP_MS;
