import { StyleSheet, Text, View } from "react-native";

import { colors, metrics, typeScale } from "../tokens";

/**
 * A labelled divider between list blocks — the mobile counterpart of the
 * desktop sidebar's "Pinned" divider (`WorkspaceSidebar.tsx`): meta-size
 * uppercase label in `--chrome-dim` on `--chrome-bg`, hairline above.
 */
export function ListDivider({ label }: { label: string }) {
  return (
    <View style={styles.divider}>
      <Text style={styles.label}>{label.toUpperCase()}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  divider: {
    backgroundColor: colors.chromeBg,
    borderTopColor: colors.chromeHairline,
    // The same rule `ListRow` draws under itself, laid over the row's own
    // bottom rule: one hairline between a row and a divider, not two.
    borderTopWidth: metrics.hairlineWidth,
    marginTop: -metrics.hairlineWidth,
    paddingBottom: 6,
    paddingHorizontal: 16,
    paddingTop: 14,
  },
  label: { color: colors.chromeDim, fontSize: typeScale.meta, fontWeight: "600", letterSpacing: 0.6 },
});
