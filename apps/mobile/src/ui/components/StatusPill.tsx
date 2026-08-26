import { StyleSheet, Text, View } from "react-native";

import type { AgentDisplayState } from "../../store/selectors";
import { colors, radii, terminalTheme, typeScale } from "../tokens";

export type PillState = AgentDisplayState | "gone";

/** The agent status pill (design.md §9.3.1), 12 sp, exact labels and colours. */
export function StatusPill({ state }: { state: PillState }) {
  const { background, ink, label } = PILLS[state];
  return (
    <View style={[styles.pill, { backgroundColor: background }]}>
      <Text style={[styles.label, { color: ink }]}>{label}</Text>
    </View>
  );
}

const PILLS: Record<PillState, { background: string; ink: string; label: string }> = {
  blocked: { background: colors.danger, ink: colors.accentInk, label: "Needs you" },
  done: { background: terminalTheme.green, ink: colors.accentInk, label: "Done" },
  working: { background: colors.accentWash, ink: colors.accent, label: "Working" },
  idle: { background: colors.chromeSelected, ink: colors.chromeDim, label: "Idle" },
  unknown: { background: colors.chromeSelected, ink: colors.chromeDim, label: "Unknown" },
  gone: { background: colors.chromeSelected, ink: colors.chromeDim, label: "Gone" },
};

const styles = StyleSheet.create({
  pill: {
    borderRadius: radii.pill,
    paddingHorizontal: 8,
    paddingVertical: 3,
  },
  label: {
    fontSize: typeScale.meta,
    fontWeight: "600",
  },
});
