// The app's foreground state as the terminal controller reads it (design doc
// §7.6 step 5, D6). Kept out of the controller so its tests run in node.

import { AppState } from "react-native";
import type { AppForeground } from "./TerminalController";

export const appForeground: AppForeground = {
  // Not `=== "active"`: before the first native read `currentState` can be
  // `unknown`, and a screen that attaches then must still be sized.
  inForeground: () => AppState.currentState !== "background",
  onForeground: (listener) => {
    const subscription = AppState.addEventListener("change", (next) => {
      if (next === "active") listener();
    });
    return () => subscription.remove();
  },
};
