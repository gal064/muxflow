import { StyleSheet, Text, View } from "react-native";

import { colors, metrics, typeScale } from "../tokens";

/**
 * A labelled divider between list blocks — the mobile counterpart of the
 * desktop sidebar's "Pinned" divider (`WorkspaceSidebar.tsx`): meta-size
 * uppercase label in `--chrome-dim` on `--chrome-bg`, hairline above.
 */
export function ListDivider({ label, afterRow, first }: {
  label: string;
  /** A `ListRow` sits directly above: lay this rule over the row's bottom rule, so the two draw as one hairline. */
  afterRow?: boolean;
  /** Opens the list: no rule (nothing above to divide from) and less air, the desktop's `.list-block:first-child > .list-divider`. */
  first?: boolean;
}) {
  return (
    <View accessibilityLabel={label} accessibilityRole="header" style={[styles.divider, afterRow && styles.overlapRow, first && styles.first]}>
      <Text style={styles.label}>{label.toUpperCase()}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  divider: {
    backgroundColor: colors.chromeBg,
    borderTopColor: colors.chromeHairline,
    // The same rule `ListRow` draws under itself.
    borderTopWidth: metrics.hairlineWidth,
    paddingBottom: 6,
    paddingHorizontal: 16,
    paddingTop: 14,
  },
  overlapRow: { marginTop: -metrics.hairlineWidth },
  first: { borderTopWidth: 0, paddingTop: 10 },
  label: { color: colors.chromeDim, fontSize: typeScale.meta, fontWeight: "600", letterSpacing: 0.6 },
});
