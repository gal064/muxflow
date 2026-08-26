import { Link } from "expo-router";
import { StyleSheet, Text, View } from "react-native";

import { colors, radii, typeScale } from "../src/ui/tokens";

/**
 * Hosts — design.md §9.1. M0 renders the empty state only; the host list, the
 * long-press sheet and the FAB arrive with M6.
 */
export default function HostsScreen() {
  return (
    <View style={styles.root}>
      <Text style={styles.glyph}>{">_"}</Text>
      <Text style={styles.heading}>No hosts yet</Text>
      <Text style={styles.body}>
        Add the machine where Muxflow desktop runs. The app connects over SSH, the same way you
        would from a terminal.
      </Text>
      <Link href="/hosts/new" style={styles.button}>
        Add host
      </Link>
      <Link href="/key" style={styles.footerLink}>
        Your SSH key
      </Link>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    flex: 1,
    gap: 12,
    justifyContent: "center",
    padding: 32,
  },
  glyph: {
    color: colors.chromeFaint,
    fontSize: 40,
  },
  heading: {
    color: colors.chromeInkStrong,
    fontSize: typeScale.rowTitle,
    fontWeight: "600",
  },
  body: {
    color: colors.chromeDim,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  button: {
    backgroundColor: colors.accent,
    borderRadius: radii.card,
    color: colors.accentInk,
    fontSize: typeScale.body,
    fontWeight: "600",
    marginTop: 12,
    overflow: "hidden",
    paddingHorizontal: 20,
    paddingVertical: 12,
  },
  footerLink: {
    color: colors.accent,
    fontSize: typeScale.meta,
    marginTop: 16,
  },
});
