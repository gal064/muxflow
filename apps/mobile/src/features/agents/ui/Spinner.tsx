import { useEffect } from "react";
import { StyleSheet, View } from "react-native";
import Animated, { cancelAnimation, Easing, makeMutable, ReduceMotion, useAnimatedStyle, withRepeat, withTiming } from "react-native-reanimated";
import Svg, { Circle } from "react-native-svg";

import { SPINNER_ARC_TURNS, SPINNER_TRACK_OPACITY, SPINNER_TURN_MS } from "../spinnerSchedule";

/**
 * The one "running" mark, shared by the docked row badge and the Working
 * heading: a faint full track with a heavier arc of ink turned around it —
 * the desktop's `.spinner`, whose 1.5 px border on a 9 px ring is the
 * stroke-to-size proportion kept here.
 *
 * The turn is continuous — the desktop's `.9s linear infinite` — and runs on
 * the UI thread (reanimated), so the JS thread never wakes for it; the arc is
 * a hardware-textured layer, so each frame is a transform of a cached bitmap
 * rather than a redraw of the SVG. (A stepped turn was tried to save repaints
 * on the software-rendered emulator; on a phone it read as lag.)
 *
 * Every spinner reads one module-wide clock rather than owning a timer, so
 * five working rows turn on the same frame and cost what one does; the clock
 * runs while any spinner with `animate` is mounted and freezes where it is
 * when the last one leaves (the list is still on screen for a moment as the
 * Terminal screen pushes over it, and a snap to 12 o'clock would show). The
 * next start begins the turn at 12 again.
 * `animate` false means nobody can see it (unfocused tab, backgrounded app)
 * or the user asked for reduced motion — a still arc is the desktop's
 * reduced-motion rendering too.
 */
export function Spinner({ ink, ring, size, animate }: { ink: string; ring: string; size: number; animate: boolean }) {
  useEffect(() => (animate ? acquireClock() : undefined), [animate]);
  const rotation = useAnimatedStyle(() => ({ transform: [{ rotate: `${clock.value}deg` }] }));
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

/** The one rotation, in degrees, every mounted spinner draws. */
const clock = makeMutable(0);
let clockUsers = 0;

/**
 * Counts a spinner in; the first one starts the clock from 0 and the last one
 * out stops it where it is. `ReduceMotion.System` is reanimated's own
 * synchronous read of the OS setting: the caller's `animate` (read from
 * `AccessibilityInfo` asynchronously) catches later changes, this catches the
 * first frames before that promise settles.
 */
function acquireClock(): () => void {
  clockUsers += 1;
  if (clockUsers === 1) {
    clock.value = 0;
    clock.value = withRepeat(
      withTiming(360, { duration: SPINNER_TURN_MS, easing: Easing.linear, reduceMotion: ReduceMotion.System }),
      -1,
      false,
      undefined,
      ReduceMotion.System,
    );
  }
  return () => {
    clockUsers -= 1;
    if (clockUsers === 0) cancelAnimation(clock);
  };
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
