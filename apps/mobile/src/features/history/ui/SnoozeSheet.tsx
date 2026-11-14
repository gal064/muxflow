import { Pressable, StyleSheet, Text } from "react-native";

import { Hairline } from "../../../ui/components/Button";
import { Sheet } from "../../../ui/components/Sheet";
import { colors, typeScale } from "../../../ui/tokens";
import { snoozeChoices } from "../historyModel";

/**
 * Snooze (chat-mock.html, "Snooze"): close the session now and bring it
 * back at a chosen time. The desktop's default intervals; picking a time of
 * your own is the desktop's.
 */
export function SnoozeSheet({ visible, title, now, onDismiss, onPick }: { visible: boolean; title: string | undefined; now: number; onDismiss: () => void; onPick: (untilMs: number, label: string) => void }) {
  const choices = snoozeChoices(now);
  return (
    <Sheet onDismiss={onDismiss} title={title ? `Snooze ${title}` : "Snooze"} visible={visible}>
      <Text style={styles.help}>Closes the session now. It comes back in History when it wakes.</Text>
      {choices.map((choice, index) => (
        <Pressable accessibilityRole="button" key={choice.label} onPress={() => onPick(choice.untilMs, choice.label)} style={({ pressed }) => [styles.action, index > 0 && styles.divided, pressed && styles.pressed]}>
          <Text style={styles.label}>{choice.label}</Text>
        </Pressable>
      ))}
      <Hairline />
    </Sheet>
  );
}

const styles = StyleSheet.create({
  help: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, lineHeight: 18, paddingBottom: 6 },
  action: { justifyContent: "center", minHeight: 52 },
  divided: { borderTopColor: colors.chromeHairline, borderTopWidth: 1 },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle },
  pressed: { opacity: 0.75 },
});
