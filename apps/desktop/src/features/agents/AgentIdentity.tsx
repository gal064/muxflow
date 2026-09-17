import { AgentStateIndicator } from "../../ui/AgentStateIndicator";
import type { AgentAdapterId, AgentDisplayState } from "./types";

/**
 * Compact, decorative adapter marks; the adjacent text carries the name.
 *
 * The two marks are drawn to the same optical weight rather than to the same
 * box. Claude's is four strokes at width 2 — the eight-spoke asterisk it was
 * turned into ink that never resolved at 12px, so it read as a smudge next to
 * Codex's solid blossom. The blossom in turn is inset ~14%: it fills its
 * viewBox edge to edge, so at a shared 12px it carried noticeably more mass
 * than any stroked mark could. Neither path is redrawn — one is simplified to
 * its four longest spokes, the other is scaled about its own centre.
 */
export function AgentIcon({ adapterId }: { adapterId: AgentAdapterId }) {
  if (adapterId === "claude-code") {
    return <svg aria-hidden="true" className="agent-icon claude" data-agent-icon="claude" viewBox="0 0 16 16">
      <path d="M8 1.5v13M1.5 8h13M3.4 3.4l9.2 9.2M12.6 3.4l-9.2 9.2" />
    </svg>;
  }
  if (adapterId === "codex") {
    return <svg aria-hidden="true" className="agent-icon codex" data-agent-icon="codex" data-icon-source="openai-blossom" viewBox="146.694 227.042 267.198 264.812">
      {/* Official OpenAI Blossom geometry, scaled by the viewBox rather than
          altered. The wrapping transform is an optical inset about the
          viewBox's own centre — the path itself is byte-for-byte the original. */}
      <g transform="translate(280.293 359.448) scale(.86) translate(-280.293 -359.448)">
        <path d="M249.176 323.434V298.276C249.176 296.158 249.971 294.569 251.825 293.509L302.406 264.381C309.29 260.409 317.5 258.555 325.973 258.555C357.75 258.555 377.877 283.185 377.877 309.399C377.877 311.253 377.877 313.371 377.611 315.49L325.178 284.771C322.001 282.919 318.822 282.919 315.645 284.771L249.176 323.434ZM367.283 421.415V361.301C367.283 357.592 365.694 354.945 362.516 353.092L296.048 314.43L317.763 301.982C319.617 300.925 321.206 300.925 323.058 301.982L373.639 331.112C388.205 339.586 398.003 357.592 398.003 375.069C398.003 395.195 386.087 413.733 367.283 421.412V421.415ZM233.553 368.452L211.838 355.742C209.986 354.684 209.19 353.095 209.19 350.975V292.718C209.19 264.383 230.905 242.932 260.301 242.932C271.423 242.932 281.748 246.641 290.49 253.26L238.321 283.449C235.146 285.303 233.555 287.951 233.555 291.659V368.455L233.553 368.452ZM280.292 395.462L249.176 377.985V340.913L280.292 323.436L311.407 340.913V377.985L280.292 395.462ZM300.286 475.968C289.163 475.968 278.837 472.259 270.097 465.64L322.264 435.449C325.441 433.597 327.03 430.949 327.03 427.239V350.445L349.011 363.155C350.865 364.213 351.66 365.802 351.66 367.922V426.179C351.66 454.514 329.679 475.965 300.286 475.965V475.968ZM237.525 416.915L186.944 387.785C172.378 379.31 162.582 361.305 162.582 343.827C162.582 323.436 174.763 305.164 193.563 297.485V357.861C193.563 361.571 195.154 364.217 198.33 366.071L264.535 404.467L242.82 416.915C240.967 417.972 239.377 417.972 237.525 416.915ZM234.614 460.343C204.689 460.343 182.71 437.833 182.71 410.028C182.71 407.91 182.976 405.792 183.238 403.672L235.405 433.863C238.582 435.715 241.763 435.715 244.938 433.863L311.407 395.466V420.622C311.407 422.742 310.612 424.331 308.758 425.389L258.179 454.519C251.293 458.491 243.083 460.343 234.611 460.343H234.614ZM300.286 491.854C332.329 491.854 359.073 469.082 365.167 438.892C394.825 431.211 413.892 403.406 413.892 375.073C413.892 356.535 405.948 338.529 391.648 325.552C392.972 319.991 393.766 314.43 393.766 308.87C393.766 271.003 363.048 242.666 327.562 242.666C320.413 242.666 313.528 243.723 306.644 246.109C294.725 234.457 278.307 227.042 260.301 227.042C228.258 227.042 201.513 249.815 195.42 280.004C165.761 287.685 146.694 315.49 146.694 343.824C146.694 362.362 154.638 380.368 168.938 393.344C167.613 398.906 166.819 404.467 166.819 410.027C166.819 447.894 197.538 476.231 233.024 476.231C240.172 476.231 247.058 475.173 253.943 472.788C265.859 484.441 282.278 491.854 300.286 491.854Z" />
      </g>
    </svg>;
  }
  return <svg aria-hidden="true" className="agent-icon generic" data-agent-icon="agent" viewBox="0 0 16 16">
    <circle cx="8" cy="8" r="5.5" /><circle cx="6" cy="7" r=".8" /><circle cx="10" cy="7" r=".8" /><path d="M5.5 10h5" />
  </svg>;
}

/**
 * An adapter mark with its agent's state docked to it.
 *
 * Every list row used to spend two slots on one agent — a state dot, then an
 * icon — which at 11.5px meant a third of the row's width was indicators
 * before a name started. Docking the state to the mark makes it one object:
 * *which* agent and *how* it is doing, read in a single fixation, the way a
 * presence dot works on an avatar.
 *
 * With the glyph option on it goes back to two marks. A 6px badge cannot hold
 * a legible "!" or "✓", and the option exists precisely so state does not
 * depend on colour, so the full-size dot is the honest fallback rather than a
 * shrunken one.
 *
 * Both marks still live inside the one `.agent-mark` wrapper, so this is always
 * a single node however it is drawn. Returning a bare fragment made the compact
 * workspace cluster — a flex row with one uniform gap — render dot, icon, dot,
 * icon at that same gap: no pairing was visible, and three agents took the
 * width of six marks.
 */
export function AgentMark({ adapterId, state, glyphs }: {
  adapterId: AgentAdapterId;
  state: AgentDisplayState;
  glyphs: boolean;
}) {
  if (glyphs) {
    return <span className="agent-mark agent-mark-glyphs">
      <AgentStateIndicator glyphs state={state} />
      <AgentIcon adapterId={adapterId} />
    </span>;
  }
  return <span className="agent-mark">
    <AgentIcon adapterId={adapterId} />
    {/* `spinner` on the working badge is the same class the tab strip and the
        workspace title use, so there is one ring animation in the app. Idle
        draws no badge at all: quiet is the resting state, and a mark with
        nothing docked to it says "nothing needs you" better than any outline. */}
    {state !== "idle"
      && <span aria-hidden="true" className={state === "working" ? "spinner agent-mark-badge working" : `agent-mark-badge ${state}`} />}
  </span>;
}
