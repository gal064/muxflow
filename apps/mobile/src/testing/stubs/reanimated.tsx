// `react-native-reanimated` for vitest (see reactNative.tsx): nothing moves.

import { Animated } from "./reactNative";

export function makeMutable<T>(value: T) {
  return { value };
}
export function useAnimatedStyle() {
  return {};
}
export function withRepeat<T>(value: T) {
  return value;
}
export function withTiming<T>(value: T) {
  return value;
}
export function cancelAnimation() {}
export const Easing = { linear: () => 0, bezier: () => () => 0, inOut: (fn: unknown) => fn };
export const ReduceMotion = { System: "system", Always: "always", Never: "never" };
export default { View: Animated.View };
