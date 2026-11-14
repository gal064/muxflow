// `expo-router` for vitest (see reactNative.tsx). Screens rendered in tests are always focused.

import { useEffect } from "react";

export const router = { push() {}, replace() {}, back() {}, canGoBack: () => true };
export function useRouter() {
  return router;
}
export function useIsFocused() {
  return true;
}
export function useFocusEffect(effect: () => void | (() => void)) {
  useEffect(effect, [effect]);
}
export function useLocalSearchParams() {
  return {};
}
export function Redirect() {
  return null;
}
export const Stack = { Screen: () => null };
