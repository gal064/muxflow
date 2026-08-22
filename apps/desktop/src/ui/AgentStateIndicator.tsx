import type { AgentDisplayState } from "../features/agents/types";
import { StateDot } from "./StateDot";

/**
 * One agent's state, drawn the same way everywhere it appears.
 *
 * Three surfaces had each answered "how do I show working?" on their own: the
 * tab strip spun a ring, the sidebar's agent rows drew a filled blue dot, and
 * a workspace row drew a filled blue dot under a spinning ring. A dot is a
 * state and a spinner is a process, so the same agent read as two different
 * things depending on where you looked at it.
 *
 * The rule is one line: motion means running, a dot means a state that is not
 * going anywhere. The exception is the accessibility option — a glyph needs a
 * dot to sit inside, so with `glyphs` on every state falls back to
 * {@link StateDot} and the shape carries the encoding, which is the whole point
 * of the option.
 */
export function AgentStateIndicator({ state, glyphs, className, spinnerClassName, label }: {
  state: AgentDisplayState;
  /** The user's accessibility preference; `forced-colors` overrides it. */
  glyphs: boolean;
  /** `state-dot` in lists, `tab-dot` in the tab strip — same encoding, two sizes. */
  className?: string;
  /** Sizing/colour class for the spinner branch, beside the shared `spinner`. */
  spinnerClassName?: string;
  /** Set only where the surrounding row does *not* already name the state. */
  label?: string;
}) {
  if (state === "working" && !glyphs) {
    return <span
      aria-hidden={label ? undefined : "true"}
      aria-label={label}
      className={spinnerClassName ? `spinner ${spinnerClassName}` : "spinner"}
      role={label ? "img" : undefined}
    />;
  }
  return <StateDot className={className} glyphs={glyphs} label={label} state={state} />;
}
