import { useEffect, useRef } from "react";
import { Animated, Easing, StyleSheet, View } from "react-native";
import Svg, { Circle } from "react-native-svg";

import type { AgentDisplayState } from "../../../store/selectors";
import { colors, metrics, radii } from "../../../ui/tokens";
import { AgentIcon } from "./AgentIcon";

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
 */
export function AgentMark({ adapterId, state }: { adapterId: string; state: AgentDisplayState }) {
  return (
    <View importantForAccessibility="no-hide-descendants" style={styles.avatar}>
      <AgentIcon adapterId={adapterId} color={colors.chromeInk} size={ICON_SIZE} />
      {state !== "idle" ? <View style={styles.dock}><StateBadge ring={colors.chromeBg} state={state} /></View> : null}
    </View>
  );
}

/**
 * The state mark on its own, for the priority headings: same encoding as the
 * docked badge, at a size that labels a group rather than reports on one agent,
 * in the heading's own ink. Idle holds its slot invisibly so the label column
 * stays aligned.
 */
export function StateBadge({ state, ring, size = BADGE_SIZE, ink = colors.chromeInk }: {
  state: AgentDisplayState;
  ring: string;
  size?: number;
  /** The spinner's colour; the static dots keep their state colours. */
  ink?: string;
}) {
  if (state === "working") return <Spinner ink={ink} ring={ring} size={size + 1} />;
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
          <Circle cx={5} cy={5} fill="none" r={4.25} stroke={colors.chromeDim} strokeDasharray="2 2" strokeWidth={1.5} />
        </Svg>
        : null}
    </View>
  );
}

/**
 * One rotating ring, the same one the desktop draws everywhere it says
 * "running": a faint full track with a quarter arc of ink, turned once every
 * 0.9 s. Drawn as SVG so the arc stays anti-aliased under rotation.
 */
function Spinner({ ink, ring, size }: { ink: string; ring: string; size: number }) {
  const turn = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    const loop = Animated.loop(Animated.timing(turn, { toValue: 1, duration: 900, easing: Easing.linear, useNativeDriver: true }));
    loop.start();
    return () => loop.stop();
  }, [turn]);
  const inner = size - 2 * RING_WIDTH;
  return (
    <View style={[styles.ring, { width: size, height: size, borderRadius: size / 2, backgroundColor: ring }]}>
      <Animated.View
        renderToHardwareTextureAndroid
        style={{ width: inner, height: inner, transform: [{ rotate: turn.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "360deg"] }) }] }}
      >
        <Svg height={inner} viewBox="0 0 12 12" width={inner}>
          <Circle cx={6} cy={6} fill="none" r={SPIN_R} stroke={ink} strokeOpacity={0.35} strokeWidth={1.5} />
          <Circle cx={6} cy={6} fill="none" r={SPIN_R} stroke={ink} strokeDasharray={`${SPIN_ARC} ${SPIN_CIRC - SPIN_ARC}`} strokeLinecap="round" strokeWidth={1.5} />
        </Svg>
      </Animated.View>
    </View>
  );
}

const ICON_SIZE = 20;
/** Outer badge size including its ring; the desktop's 6px dot in a 1.5px ring, scaled to touch. */
const BADGE_SIZE = 13;
const RING_WIDTH = 1.5;
const SPIN_R = 4.75;
const SPIN_CIRC = 2 * Math.PI * SPIN_R;
/** A quarter turn of ink — the desktop's `border-top-color: currentColor`. */
const SPIN_ARC = SPIN_CIRC / 4;

const styles = StyleSheet.create({
  avatar: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.card,
    height: metrics.agentAvatarSize,
    justifyContent: "center",
    width: metrics.agentAvatarSize,
  },
  dock: { position: "absolute", right: -5, bottom: -5 },
  ring: { alignItems: "center", justifyContent: "center" },
  hidden: { opacity: 0 },
});
