import { Pressable, StyleSheet, Text } from "react-native";

import type { VoicePlaybackRate } from "../../store/prefsStore";
import { colors, fixedChromeText, radii, typeScale } from "../../ui/tokens";
import { formatRate, nextPlaybackRate } from "./speed";

/** One compact control that advances through the three supported playback rates. */
export function SpeedButton({ rate, onChange }: { rate: VoicePlaybackRate; onChange: (rate: VoicePlaybackRate) => void }) {
  const next = nextPlaybackRate(rate);
  return (
    <Pressable
      accessibilityHint={`Sets playback speed to ${formatRate(next)}`}
      accessibilityLabel={`Playback speed ${formatRate(rate)}`}
      accessibilityRole="button"
      hitSlop={{ top: 6, bottom: 6 }}
      onPress={() => onChange(next)}
      style={({ pressed }) => [styles.button, pressed && styles.pressed]}
    >
      <Text {...fixedChromeText} numberOfLines={1} style={styles.label}>{formatRate(rate)}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  button: { alignItems: "center", backgroundColor: colors.chromeRaised, borderRadius: radii.card, height: 40, justifyContent: "center", minWidth: 60, paddingHorizontal: 14 },
  label: { color: colors.chromeInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  pressed: { opacity: 0.75 },
});
