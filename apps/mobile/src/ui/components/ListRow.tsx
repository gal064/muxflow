import type { ReactNode } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, metrics, typeScale } from "../tokens";

export interface ListRowProps {
  title: string;
  subtitle?: string;
  height: number;
  /** 3 dp bar on the left edge (§9.3.1) in this colour, when set. */
  edgeColor?: string;
  leading?: ReactNode;
  /** Drawn right after the title text — a pin, say — so it hugs the end of the name rather than the row's edge. */
  titleAccessory?: ReactNode;
  trailing?: ReactNode;
  dimmed?: boolean;
  onPress?: () => void;
  disabled?: boolean;
  titleColor?: string;
}

/** A list row per §9.3/§9.4: hairline-separated, title 16 sp, second line 13 sp `--chrome-dim`. */
export function ListRow({ title, subtitle, height, edgeColor, leading, titleAccessory, trailing, dimmed, onPress, disabled, titleColor }: ListRowProps) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled || !onPress}
      onPress={onPress}
      style={({ pressed }) => [styles.row, { height }, pressed && styles.pressed]}
    >
      {edgeColor ? <View style={[styles.edge, { backgroundColor: edgeColor }]} /> : null}
      <View style={[styles.body, dimmed && styles.dimmed]}>
        {leading ? <View style={styles.leading}>{leading}</View> : null}
        <View style={styles.text}>
          <View style={styles.titleLine}>
            <Text style={[styles.title, titleColor ? { color: titleColor } : null]} numberOfLines={1}>{title}</Text>
            {titleAccessory ? <View style={styles.titleAccessory}>{titleAccessory}</View> : null}
          </View>
          {subtitle ? <Text style={styles.subtitle} numberOfLines={1}>{subtitle}</Text> : null}
        </View>
        {trailing ? <View style={styles.trailing}>{trailing}</View> : null}
      </View>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  row: {
    borderBottomColor: colors.chromeHairline,
    borderBottomWidth: metrics.hairlineWidth,
    flexDirection: "row",
  },
  pressed: { backgroundColor: colors.chromeHover },
  edge: { width: metrics.attentionEdgeBarWidth },
  body: {
    alignItems: "center",
    flex: 1,
    flexDirection: "row",
    gap: 12,
    paddingHorizontal: 16,
  },
  dimmed: { opacity: 0.5 },
  leading: {},
  text: { flex: 1, gap: 2 },
  titleLine: { alignItems: "center", flexDirection: "row", gap: 6 },
  title: { color: colors.chromeInkStrong, flexShrink: 1, fontSize: typeScale.rowTitle },
  titleAccessory: { flexShrink: 0 },
  subtitle: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  trailing: { alignItems: "flex-end" },
});
