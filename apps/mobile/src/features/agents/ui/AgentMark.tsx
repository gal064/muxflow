import { StyleSheet, View } from "react-native";
import Svg, { Circle } from "react-native-svg";

import type { AgentDisplayState } from "../../../store/selectors";
import { colors, metrics, radii } from "../../../ui/tokens";
import { AgentIcon } from "./AgentIcon";
import { RING_WIDTH, Spinner } from "./Spinner";

/**
 * The adapter mark with its agent's state docked to it — the desktop's
 * `AgentMark`, at touch size. One object per agent instead of an avatar, a gap
 * and a pill: *which* agent and *how* it is doing, read in a single fixation,
 * the way a presence dot works on an avatar.
 *
 * The badge overhangs the avatar's bottom-right corner so it sits mostly on
 * the row background with the tile's corner cutting behind it; that overhang
 * is what separates it from the mark (the desktop punches a ring out of the
 * row background instead, but `--chrome-bg` on `--chrome-raised` is a 1.07:1
 * step and would not read). Blocked is a red dot, done the bright
 * notification green, unknown a dashed outline ("nothing has told us"),
 * working a spinner — motion means running, a dot means a state that is not
 * going anywhere. Idle draws no badge at all: quiet is the resting state.
 *
 * The mark is decorative: the row's accessibility label names the state.
 * `animate` is whether the working spinner may turn (see `Spinner`).
 */
export function AgentMark({ adapterId, state, animate, surface = colors.chromeRaised, ring = colors.chromeBg }: {
  adapterId: string;
  state: AgentDisplayState;
  animate: boolean;
  /** Tile surface; headers invert the list's raised-on-background treatment. */
  surface?: string;
  /** Knockout around the state badge; should match the mark's parent surface. */
  ring?: string;
}) {
  return (
    <View importantForAccessibility="no-hide-descendants" style={[styles.avatar, { backgroundColor: surface }]}>
      <AgentIcon adapterId={adapterId} color={colors.chromeInk} size={ICON_SIZE} />
      {state !== "idle" ? <View style={styles.dock}><StateBadge animate={animate} ring={ring} state={state} /></View> : null}
    </View>
  );
}

/**
 * The state mark on its own, for the priority headings: same encoding as the
 * docked badge, at a size that labels a group rather than reports on one agent,
 * in the heading's own ink. Idle holds its slot invisibly so the label column
 * stays aligned.
 */
export function StateBadge({ state, ring, animate, size = BADGE_SIZE, ink = colors.chromeInk }: {
  state: AgentDisplayState;
  ring: string;
  /** Whether a working spinner may turn; see `Spinner`. */
  animate: boolean;
  size?: number;
  /** The spinner's colour; the static dots keep their state colours. */
  ink?: string;
}) {
  if (state === "working") return <Spinner animate={animate} ink={ink} ring={ring} size={size + 1} />;
  const inner = size - 2 * RING_WIDTH;
  const dot = { width: inner, height: inner, borderRadius: inner / 2 };
  return (
    <View style={[styles.ring, { width: size, height: size, borderRadius: size / 2, backgroundColor: ring }, state === "idle" && styles.hidden]}>
      {state === "blocked" ? <View style={[dot, { backgroundColor: colors.danger }]} /> : null}
      {state === "done" ? <View style={[dot, { backgroundColor: colors.ok }]} /> : null}
      {/* SVG rather than a dashed border: Android derives the dash length from
          the border width, which on a 10 dp circle draws three coarse gaps. */}
      {state === "unknown"
        ? <Svg height={inner} viewBox="0 0 10 10" width={inner}>
          <Circle cx={5} cy={5} fill="none" r={UNKNOWN_R} stroke={colors.chromeDim} strokeDasharray={`${UNKNOWN_DASH} ${UNKNOWN_DASH}`} strokeWidth={1.5} />
        </Svg>
        : null}
    </View>
  );
}

const ICON_SIZE = 20;
/** Outer badge size including its ring; the desktop's 6px dot in a 1.5px ring, scaled to touch. */
const BADGE_SIZE = 13;
const UNKNOWN_R = 4.25;
/** Seven dashes that tile the circle exactly, so the seam at 3 o'clock does not merge two of them. */
const UNKNOWN_DASH = (2 * Math.PI * UNKNOWN_R) / 14;

const styles = StyleSheet.create({
  avatar: {
    alignItems: "center",
    borderRadius: radii.card,
    height: metrics.agentAvatarSize,
    justifyContent: "center",
    width: metrics.agentAvatarSize,
  },
  dock: { position: "absolute", right: -5, bottom: -5 },
  ring: { alignItems: "center", justifyContent: "center" },
  hidden: { opacity: 0 },
});
