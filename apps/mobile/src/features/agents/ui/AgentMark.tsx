import { useEffect, useRef } from "react";
import { Animated, Easing, StyleSheet, View } from "react-native";

import type { AgentDisplayState } from "../../../store/selectors";
import { colors, metrics, radii } from "../../../ui/tokens";
import { AgentIcon } from "./AgentIcon";

/**
 * The adapter mark with its agent's state docked to it — the desktop's
 * `AgentMark`, at touch size. One object per agent instead of an avatar, a gap
 * and a pill: *which* agent and *how* it is doing, read in a single fixation,
 * the way a presence dot works on an avatar.
 *
 * The badge sits on the avatar's bottom-right corner inside a ring of the
 * row's background, so it reads as a separate layer rather than a bite out of
 * the mark. Blocked is a red dot, done the bright notification green, unknown
 * a dashed outline ("nothing has told us"), working a spinner — motion means
 * running, a dot means a state that is not going anywhere. Idle draws no
 * badge at all: quiet is the resting state.
 */
export function AgentMark({ adapterId, state }: { adapterId: string; state: AgentDisplayState }) {
  return (
    <View style={styles.avatar}>
      <AgentIcon adapterId={adapterId} color={colors.chromeInk} size={ICON_SIZE} />
      {state !== "idle" ? <View style={styles.dock}><StateBadge ring={colors.chromeBg} state={state} /></View> : null}
    </View>
  );
}

/**
 * The state mark on its own, for the priority headings: same encoding as the
 * docked badge, at a size that labels a group rather than reports on one agent.
 * Idle holds its slot invisibly so the label column stays aligned.
 */
export function StateBadge({ state, ring, size = BADGE_SIZE }: { state: AgentDisplayState; ring: string; size?: number }) {
  if (state === "working") return <Spinner ring={ring} size={size + 1} />;
  const inner = size - 2 * RING_WIDTH;
  const dot = { width: inner, height: inner, borderRadius: inner / 2 };
  return (
    <View style={[styles.ring, { width: size, height: size, borderRadius: size / 2, backgroundColor: ring }, state === "idle" && styles.hidden]}>
      {state === "blocked" ? <View style={[dot, { backgroundColor: colors.danger }]} /> : null}
      {state === "done" ? <View style={[dot, { backgroundColor: colors.ok }]} /> : null}
      {state === "unknown" ? <View style={[dot, styles.unknown]} /> : null}
    </View>
  );
}

/** One rotating ring, the same one the desktop draws everywhere it says "running". */
function Spinner({ ring, size }: { ring: string; size: number }) {
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
        style={[
          styles.spinner,
          { width: inner, height: inner, borderRadius: inner / 2, backgroundColor: ring },
          { transform: [{ rotate: turn.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "360deg"] }) }] },
        ]}
      />
    </View>
  );
}

const ICON_SIZE = 20;
/** Outer badge size including its ring; the desktop's 6px dot in a 1.5px ring, scaled to touch. */
const BADGE_SIZE = 14;
const RING_WIDTH = 2;

const styles = StyleSheet.create({
  avatar: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.card,
    height: metrics.agentAvatarSize,
    justifyContent: "center",
    width: metrics.agentAvatarSize,
  },
  dock: { position: "absolute", right: -3, bottom: -3 },
  ring: { alignItems: "center", justifyContent: "center" },
  hidden: { opacity: 0 },
  unknown: { borderColor: colors.chromeDim, borderStyle: "dashed", borderWidth: 1.5 },
  spinner: { borderColor: colors.spinnerTrack, borderTopColor: colors.chromeInk, borderWidth: 1.5 },
});
