import { useIsFocused } from "expo-router";
import { useEffect, useState } from "react";
import { AccessibilityInfo, AppState } from "react-native";

/**
 * Whether a continuous animation on this screen is worth drawing right now:
 * the screen is the focused one, the app is in the foreground, and the user
 * has not asked the OS for reduced motion. Anything that loops — a spinner,
 * a pulse — should stop when this is false: an unfocused tab and a
 * backgrounded app still hold their views, and a loop nobody can see is pure
 * cost. Reduced motion is the desktop's `prefers-reduced-motion` gate on
 * `.spinner`: the static shape stays, the movement goes.
 *
 * Must be called under a navigator (it reads the screen's focus).
 */
export function useAnimationsAllowed(): boolean {
  const focused = useIsFocused();
  const [foreground, setForeground] = useState(() => AppState.currentState !== "background");
  const [reduceMotion, setReduceMotion] = useState(false);

  useEffect(() => {
    const subscription = AppState.addEventListener("change", (next) => setForeground(next !== "background"));
    return () => subscription.remove();
  }, []);

  useEffect(() => {
    let cancelled = false;
    void AccessibilityInfo.isReduceMotionEnabled().then((enabled) => {
      if (!cancelled) setReduceMotion(enabled);
    });
    const subscription = AccessibilityInfo.addEventListener("reduceMotionChanged", setReduceMotion);
    return () => {
      cancelled = true;
      subscription.remove();
    };
  }, []);

  return focused && foreground && !reduceMotion;
}
