import { Pressable, StyleSheet, Text, View, type StyleProp, type ViewStyle } from "react-native";

import { colors, radii, typeScale } from "../tokens";

export type ButtonVariant = "primary" | "secondary" | "danger" | "text" | "dangerText";

export interface ButtonProps {
  label: string;
  onPress: () => void;
  variant?: ButtonVariant;
  disabled?: boolean;
  style?: StyleProp<ViewStyle>;
}

/** The button shapes design.md §9 asks for: a filled `--accent` primary, a danger, and text buttons. */
export function Button({ label, onPress, variant = "primary", disabled = false, style }: ButtonProps) {
  const text = variant === "text" || variant === "dangerText";
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ disabled }}
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.base,
        text ? styles.textBase : styles.filled,
        variant === "primary" ? styles.primary : null,
        variant === "secondary" ? styles.secondary : null,
        variant === "danger" ? styles.danger : null,
        pressed && !disabled ? styles.pressed : null,
        disabled ? styles.disabled : null,
        style,
      ]}
    >
      <Text
        style={[
          styles.label,
          variant === "primary" ? styles.primaryLabel : null,
          variant === "secondary" ? styles.secondaryLabel : null,
          variant === "danger" ? styles.dangerLabel : null,
          variant === "text" ? styles.textLabel : null,
          variant === "dangerText" ? styles.dangerTextLabel : null,
        ]}
        numberOfLines={1}
      >
        {label}
      </Text>
    </Pressable>
  );
}

/** A hairline between rows, used by the sheets and lists. */
export function Hairline({ style }: { style?: StyleProp<ViewStyle> }) {
  return <View style={[styles.hairline, style]} />;
}

const styles = StyleSheet.create({
  base: {
    alignItems: "center",
    borderRadius: radii.card,
    justifyContent: "center",
  },
  filled: { paddingHorizontal: 20, paddingVertical: 13 },
  textBase: { paddingHorizontal: 8, paddingVertical: 10 },
  primary: { backgroundColor: colors.accent },
  secondary: { backgroundColor: colors.chromeSelected },
  danger: { backgroundColor: colors.danger },
  pressed: { opacity: 0.75 },
  disabled: { opacity: 0.4 },
  label: { fontSize: typeScale.rowTitle, fontWeight: "600" },
  primaryLabel: { color: colors.accentInk },
  secondaryLabel: { color: colors.chromeInkStrong },
  dangerLabel: { color: colors.chromeInkStrong },
  textLabel: { color: colors.accent },
  dangerTextLabel: { color: colors.dangerInk },
  hairline: { backgroundColor: colors.chromeHairline, height: StyleSheet.hairlineWidth },
});
