import { Linking, Pressable, StyleSheet, Text } from "react-native";
import { useStore } from "zustand";

import { updateStore } from "../updateCheck";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";

/**
 * The home app bar's red "Update" pill, shown while a newer release is
 * published. Persistent on purpose, like the desktop's: there is no dismiss,
 * it is gone once the app is updated. Tapping opens the release page.
 */
export function UpdatePill() {
  const update = useStore(updateStore, (state) => state.update);
  if (!update) return null;
  return (
    <Pressable
      accessibilityLabel={`Muxflow ${update.version} is available. Open its release page.`}
      accessibilityRole="link"
      hitSlop={8}
      onPress={() => void Linking.openURL(update.url)}
      style={styles.pill}
    >
      <Text {...fixedChromeText} style={styles.label}>↑ Update</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  pill: { backgroundColor: colors.danger, borderRadius: 12, marginRight: 4, paddingHorizontal: 10, paddingVertical: 4 },
  label: { color: "#ffffff", fontSize: typeScale.meta, fontWeight: "700" },
});
