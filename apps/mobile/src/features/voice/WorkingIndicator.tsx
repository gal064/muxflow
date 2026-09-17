import { StyleSheet, Text, View } from "react-native";

import { Spinner } from "../agents/ui/Spinner";
import { colors, metrics, radii, typeScale } from "../../ui/tokens";
import { useAnimationsAllowed } from "../../ui/useAnimationsAllowed";

/**
 * The agent's working state, in the conversation itself (design.md §9.11): a
 * typing-indicator bubble driven by the exact pane lifecycle used by the
 * header mark. It has no Voice-specific pending-message rule.
 */
export function WorkingIndicator({ working }: { working: boolean }) {
  const animate = useAnimationsAllowed();
  if (!working) return null;
  return (
    <View accessibilityLabel="Agent is working" accessibilityLiveRegion="polite" accessibilityRole="text" style={styles.row}>
      <View style={styles.bubble}>
        <Spinner animate={animate} ink={colors.chromeDim} ring={colors.chromeRaised} size={14} />
        <Text style={styles.label}>Working…</Text>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  row: { flexDirection: "row", justifyContent: "flex-start" },
  /** The agent bubble's surface and hairline (VoiceScreen `bubbleAgent`), sized to its content. */
  bubble: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: radii.card,
    borderWidth: metrics.hairlineWidth,
    flexDirection: "row",
    gap: 8,
    paddingHorizontal: 14,
    paddingVertical: 10,
  },
  label: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
});
