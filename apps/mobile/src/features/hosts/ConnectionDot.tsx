import { Pressable, StyleSheet, View } from "react-native";

import { colors } from "../../ui/tokens";
import { useConnectionSheet } from "./ConnectionSheet";
import { useSession } from "./hooks";

/**
 * The 10 dp connection dot of design.md §9.3's app bar: `--ok` connected,
 * `--warn` reconnecting, `--danger` failed. Opens the §9.8 sheet.
 */
export function ConnectionDot() {
  const state = useSession((session) => session.connection.state);
  const sheet = useConnectionSheet();
  if (state === "idle") return null;
  const color =
    state === "connected"
      ? colors.ok
      : state === "failed" || state === "incompatible"
        ? colors.danger
        : colors.warn;
  return (
    <Pressable onPress={sheet.open} hitSlop={16} accessibilityRole="button" accessibilityLabel="Connection">
      <View style={[styles.dot, { backgroundColor: color }]} />
    </Pressable>
  );
}

const styles = StyleSheet.create({
  dot: { borderRadius: 5, height: 10, marginRight: 4, width: 10 },
});
