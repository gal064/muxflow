import { StyleSheet, Text, View } from "react-native";

import { colors, typeScale } from "../tokens";

export function EmptyState({ heading, lines }: { heading: string; lines: string[] }) {
  return (
    <View style={styles.root}>
      <Text style={styles.heading}>{heading}</Text>
      {lines.map((line) => (
        <Text key={line} style={styles.body}>{line}</Text>
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  root: { alignItems: "center", flex: 1, gap: 8, justifyContent: "center", padding: 32 },
  heading: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  body: { color: colors.chromeDim, fontSize: typeScale.body, textAlign: "center" },
});
