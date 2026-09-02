import { useEffect } from "react";
import { StyleSheet, View } from "react-native";
import Animated, { cancelAnimation, Easing, ReduceMotion, useAnimatedStyle, useSharedValue, withRepeat, withTiming } from "react-native-reanimated";
import Svg, { Circle } from "react-native-svg";

import { SPINNER_ARC_TURNS, SPINNER_STEPS, SPINNER_TRACK_OPACITY, SPINNER_TURN_MS } from "../spinnerSchedule";

/**
 * The one "running" mark, shared by the docked row badge and the Working
 * heading: a faint full track with a heavier arc of ink turned around it —
 * the desktop's `.spinner`, whose 1.5 px border on a 9 px ring is the
 * stroke-to-size proportion kept here.
 *
 * The turn is stepped, not continuous. A continuous rotation asks for a
 * repaint every vsync for as long as one agent is working anywhere on the
 * list; `SPINNER_STEPS` discrete angles per turn repaint the screen ten times
 * a second instead of sixty and still read as motion. The stepping happens on
 * the UI thread (reanimated), so the JS thread never wakes for it, and the
 * arc is a hardware-textured layer, so each step is a transform of a cached
 * bitmap rather than a redraw of the SVG.
 *
 * `animate` false freezes the arc where it is: the caller says when nobody
 * can see it (unfocused tab, backgrounded app) or when the user asked for
 * reduced motion — a still arc is the desktop's reduced-motion rendering too.
 */
export function Spinner({ ink, ring, size, animate }: { ink: string; ring: string; size: number; animate: boolean }) {
  const turn = useSharedValue(0);
  useEffect(() => {
    if (!animate) {
      cancelAnimation(turn);
      return;
    }
    turn.value = 0;
    turn.value = withRepeat(
      withTiming(360, { duration: SPINNER_TURN_MS, easing: Easing.steps(SPINNER_STEPS, false), reduceMotion: ReduceMotion.Never }),
      -1,
      false,
      undefined,
      ReduceMotion.Never,
    );
    return () => cancelAnimation(turn);
  }, [animate, turn]);
  const rotation = useAnimatedStyle(() => ({ transform: [{ rotate: `${turn.value}deg` }] }));
  const inner = size - 2 * RING_WIDTH;
  return (
    <View style={[styles.ring, { width: size, height: size, borderRadius: size / 2, backgroundColor: ring }]}>
      <Animated.View renderToHardwareTextureAndroid style={[{ width: inner, height: inner }, rotation]}>
        <Svg height={inner} viewBox={`0 0 ${BOX} ${BOX}`} width={inner}>
          <Circle cx={BOX / 2} cy={BOX / 2} fill="none" r={RADIUS} stroke={ink} strokeOpacity={SPINNER_TRACK_OPACITY} strokeWidth={TRACK_STROKE} />
          <Circle
            cx={BOX / 2}
            cy={BOX / 2}
            fill="none"
            r={RADIUS}
            stroke={ink}
            strokeDasharray={`${ARC} ${CIRCUMFERENCE - ARC}`}
            strokeLinecap="round"
            strokeWidth={ARC_STROKE}
            // A dash starts at 3 o'clock; the resting arc (reduced motion, a paused tab) should sit at 12 like a clock hand.
            transform={`rotate(-90 ${BOX / 2} ${BOX / 2})`}
          />
        </Svg>
      </Animated.View>
    </View>
  );
}

/** The knockout ring around the badge, matching `StateBadge`'s. */
export const RING_WIDTH = 1.5;

const BOX = 12;
/** The desktop's 1.5 px on 9 px: a sixth of the box. */
const ARC_STROKE = BOX / 6;
const TRACK_STROKE = 1.25;
/** Centred so the arc's outer edge lands on the box's edge. */
const RADIUS = BOX / 2 - ARC_STROKE / 2;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;
const ARC = CIRCUMFERENCE * SPINNER_ARC_TURNS;

const styles = StyleSheet.create({
  ring: { alignItems: "center", justifyContent: "center" },
});
