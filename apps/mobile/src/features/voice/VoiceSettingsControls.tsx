import { Pressable, StyleSheet, Text, View } from "react-native";
import { useStore } from "zustand";

import { prefsStore } from "../../store/prefsStore";
import { colors, fixedChromeText, radii, typeScale } from "../../ui/tokens";
import { SpeedButton } from "./SpeedButton";

/**
 * The talk pane's Speed and Autoplay (design.md §9.11). One setting each,
 * shared by the Voice screen and a chat's voice mode.
 */
export function VoiceSettingsControls() {
  const playbackRate = useStore(prefsStore, (s) => s.voicePlaybackRate);
  const autoPlay = useStore(prefsStore, (s) => s.voiceAutoPlay);
  return (
    <>
      <View style={styles.speedGroup}>
        <Text {...fixedChromeText} numberOfLines={1} style={styles.speedCaption}>Speed</Text>
        <SpeedButton onChange={(rate) => prefsStore.getState().setVoicePlaybackRate(rate)} rate={playbackRate} />
      </View>
      <Pressable
        accessibilityLabel="Autoplay replies"
        accessibilityRole="switch"
        accessibilityState={{ checked: autoPlay }}
        hitSlop={{ top: 6, bottom: 6 }}
        onPress={() => prefsStore.getState().setVoiceAutoPlay(!autoPlay)}
        style={({ pressed }) => [styles.autoPlayToggle, autoPlay && styles.autoPlayToggleOn, pressed && styles.pressed]}
      >
        <Text {...fixedChromeText} numberOfLines={1} style={[styles.label, autoPlay && styles.labelOn]}>Autoplay {autoPlay ? "On" : "Off"}</Text>
      </Pressable>
    </>
  );
}

const styles = StyleSheet.create({
  speedGroup: { alignItems: "center", flexDirection: "row", gap: 8 },
  speedCaption: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  autoPlayToggle: { alignItems: "center", backgroundColor: colors.chromeRaised, borderRadius: radii.card, height: 40, justifyContent: "center", minWidth: 98, paddingHorizontal: 10 },
  autoPlayToggleOn: { backgroundColor: colors.accent },
  label: { color: colors.chromeInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  labelOn: { color: colors.accentInk },
  pressed: { opacity: 0.75 },
});
