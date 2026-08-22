import type { AgentDisplayState } from "../features/agents/types";

/**
 * The one agent-state dot.
 *
 * The plan asks for "color-only dots by default with a shape-glyph option for
 * accessibility". It was three separate spans before — the sidebar's, the tab
 * strip's, and the workspace switcher's — and only the sidebar's had ever heard
 * of the option, so turning it on left two of the three surfaces encoding state
 * as colour alone.
 *
 * The glyph is always in the DOM and hidden by CSS, which is what lets
 * `forced-colors` turn it on by itself: there the system palette replaces every
 * background, so four differently-coloured dots collapse into one shape and the
 * shape is the only thing left that can tell them apart.
 *
 * Decorative by construction: the row around it already names the state, and a
 * `role="img"` here announced it twice.
 */
const GLYPH: Record<AgentDisplayState, string> = {
  blocked: "!",
  done: "✓",
  working: "•",
  unknown: "?",
  idle: "",
};

export function StateDot({ state, glyphs, className = "state-dot", label }: {
  state: AgentDisplayState;
  /** The user's accessibility preference; `forced-colors` overrides it. */
  glyphs: boolean;
  /** `state-dot` in lists, `tab-dot` in the tab strip — same encoding, two sizes. */
  className?: string;
  /**
   * Set only where the surrounding row does *not* already name the state. In
   * the sidebar it does, and labelling here announced it twice; on a tab, the
   * dot is the only thing that says an agent wants attention.
   */
  label?: string;
}) {
  // Idle draws nothing — quiet is the resting state, the rule the sidebar's
  // mark badge already follows. The span stays and the stylesheet hides it, so
  // the slot keeps its width and nothing beside it moves when the state later
  // becomes a spinner or a dot. Decorative even where the caller passes a
  // label: there is no mark here to name.
  if (state === "idle") return <span aria-hidden="true" className={`${className} idle`} />;
  return <span
    aria-hidden={label ? undefined : "true"}
    aria-label={label}
    className={`${className} ${state}${glyphs ? " glyphs" : ""}`}
    role={label ? "img" : undefined}
  >{GLYPH[state]}</span>;
}
