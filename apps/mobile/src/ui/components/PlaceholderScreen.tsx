import { StyleSheet, Text, View } from "react-native";

import { colors, fonts, typeScale } from "../tokens";

export type PlaceholderScreenProps = {
  /** The screen's title, as spelled in design.md §9. */
  title: string;
  /** The route pattern this screen answers, shown so the skeleton is navigable. */
  route: string;
};

/**
 * M0 placeholder. Every route in design.md §9 renders one of these in the
 * chrome colours; no screen has any logic yet. Later milestones replace these
 * bodies one screen at a time.
 */
export function PlaceholderScreen({ title, route }: PlaceholderScreenProps) {
  return (
    <View style={styles.root}>
      <Text style={styles.title}>{title}</Text>
      <Text style={styles.route}>{route}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    flex: 1,
    gap: 8,
    justifyContent: "center",
    padding: 24,
  },
  title: {
    color: colors.chromeInkStrong,
    fontSize: typeScale.appBarTitle,
    fontWeight: "600",
  },
  route: {
    color: colors.chromeDim,
    fontFamily: fonts.mono,
    fontSize: typeScale.keyMono,
  },
});
