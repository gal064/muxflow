import { Pressable, ScrollView, StyleSheet, Text } from "react-native";

import { ShiftIcon } from "../../ui/components/MediaIcons";
import { colors, fixedChromeText, metrics, radii, typeScale } from "../../ui/tokens";
import { KEY_CHIPS, SHIFT_CHIP, type KeyChip } from "./chips";

/** The terminal's key chips (design.md §9.5), over its reply field. */
export function KeyChips({ shiftArmed, disabled, onChip }: { shiftArmed: boolean; disabled: boolean; onChip: (chip: KeyChip) => void }) {
  return (
    <ScrollView contentContainerStyle={styles.chips} horizontal keyboardShouldPersistTaps="always" showsHorizontalScrollIndicator={false} style={styles.row}>
      {KEY_CHIPS.map((chip) => {
        const armed = chip === SHIFT_CHIP && shiftArmed;
        return (
          <Pressable
            accessibilityLabel={chip === SHIFT_CHIP ? "Shift" : chip.label}
            accessibilityRole="button"
            accessibilityState={chip === SHIFT_CHIP ? { selected: shiftArmed } : undefined}
            disabled={disabled}
            key={chip.label}
            onPress={() => onChip(chip)}
            style={({ pressed }) => [styles.chip, armed && styles.chipArmed, pressed && styles.chipPressed, disabled && styles.disabled]}
          >
            {chip === SHIFT_CHIP
              ? <ShiftIcon color={armed ? colors.accentInk : colors.chromeInkStrong} />
              : <Text {...fixedChromeText} numberOfLines={1} style={[styles.label, chip.label.length === 1 && styles.glyph]}>{chip.label}</Text>}
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  // Edge to edge: the composer's side padding is taken back, so the row scrolls under the screen's edges.
  row: { flexGrow: 0, height: metrics.keyChipRowHeight, marginHorizontal: -10 },
  chips: { alignItems: "center", gap: 8, paddingHorizontal: 10 },
  chip: {
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: radii.pill,
    borderWidth: 1,
    height: 30,
    justifyContent: "center",
    minWidth: 44,
    paddingHorizontal: 12,
  },
  chipPressed: { backgroundColor: colors.chromeBorder },
  /** The ⇧ chip while Shift is armed for the next chip. */
  chipArmed: { backgroundColor: colors.accent, borderColor: colors.accent },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowSecondary, textAlign: "center" },
  /** Single-glyph chips (arrows, y, n) read lighter than words at 13 sp; bump them to match. */
  glyph: { fontSize: 16, fontWeight: "600" },
  disabled: { opacity: 0.4 },
});
