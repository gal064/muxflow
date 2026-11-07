import { Pressable, StyleSheet, Text } from "react-native";
import { useStore } from "zustand";

import { installAppUpdate } from "..";
import { updateStore } from "../updateCheck";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";

/**
 * The home app bar's red "Update" pill, shown while a newer release is
 * published. Persistent on purpose, like the desktop's: there is no dismiss,
 * it is gone once the app is updated. Tapping downloads and installs the update.
 */
export function UpdatePill() {
  const update = useStore(updateStore, (state) => state.update);
  const downloading = useStore(updateStore, (state) => state.downloading);
  if (!update) return null;
  return (
    <Pressable
      accessibilityLabel={downloading ? `Downloading Muxflow ${update.version}.` : `Muxflow ${update.version} is available. Install the update.`}
      accessibilityRole="button"
      disabled={downloading}
      hitSlop={8}
      onPress={() => void installAppUpdate()}
      style={styles.pill}
    >
      <Text {...fixedChromeText} style={styles.label}>{downloading ? "Downloading…" : "↑ Update"}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  pill: { backgroundColor: colors.danger, borderRadius: 12, marginRight: 4, paddingHorizontal: 10, paddingVertical: 4 },
  label: { color: "#ffffff", fontSize: typeScale.meta, fontWeight: "700" },
});
