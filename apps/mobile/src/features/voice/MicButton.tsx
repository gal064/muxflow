import { useEffect, useRef } from "react";
import { ActivityIndicator, Animated, Easing, Pressable, StyleSheet, Text, View } from "react-native";

import { MicIcon } from "../../ui/components/MediaIcons";
import { colors, typeScale } from "../../ui/tokens";
import type { VoicePhase } from "./voiceStore";

export interface MicButtonProps {
  phase: VoicePhase;
  disabled: boolean;
  /** Why it is disabled, shown under the button; empty when enabled. */
  hint: string;
  onPressIn: () => void;
  onPressOut: () => void;
}

const SIZE = 96;

/** The press-and-hold mic (design.md §9.11): pulsing ring while recording, spinner while the host works. */
export function MicButton({ phase, disabled, hint, onPressIn, onPressOut }: MicButtonProps) {
  const pulse = useRef(new Animated.Value(0)).current;
  const recording = phase === "recording";
  const busy = phase === "transcribing" || phase === "sending";

  useEffect(() => {
    if (!recording) {
      pulse.stopAnimation();
      pulse.setValue(0);
      return;
    }
    const loop = Animated.loop(Animated.sequence([
      Animated.timing(pulse, { toValue: 1, duration: 900, easing: Easing.out(Easing.quad), useNativeDriver: true }),
      Animated.timing(pulse, { toValue: 0, duration: 0, useNativeDriver: true }),
    ]));
    loop.start();
    return () => loop.stop();
  }, [pulse, recording]);

  const ringStyle = {
    opacity: pulse.interpolate({ inputRange: [0, 1], outputRange: [0.6, 0] }),
    transform: [{ scale: pulse.interpolate({ inputRange: [0, 1], outputRange: [1, 1.6] }) }],
  };
  const label = recording ? "Listening…" : phase === "transcribing" ? "Transcribing…" : phase === "sending" ? "Sending…" : "Hold to talk";

  return (
    <View style={styles.root}>
      <View style={styles.stage}>
        {recording ? <Animated.View pointerEvents="none" style={[styles.ring, ringStyle]} /> : null}
        <Pressable
          accessibilityHint="Press and hold, speak, then release to send"
          accessibilityLabel={label}
          accessibilityRole="button"
          accessibilityState={{ disabled: disabled || busy, busy }}
          disabled={disabled || busy}
          onPressIn={onPressIn}
          onPressOut={onPressOut}
          // A thumb drifting off the disc mid-sentence must not count as a release.
          pressRetentionOffset={64}
          style={[styles.button, recording && styles.buttonRecording, disabled && styles.buttonDisabled]}
        >
          {busy ? <ActivityIndicator color={colors.accentInk} size="large" /> : <MicIcon color={colors.accentInk} size={44} />}
        </Pressable>
      </View>
      <Text accessibilityLiveRegion="polite" style={styles.label}>{label}</Text>
      {disabled && hint ? <Text style={styles.hint}>{hint}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create({
  root: { alignItems: "center", gap: 8, paddingBottom: 12, paddingTop: 8 },
  stage: { alignItems: "center", height: SIZE + 24, justifyContent: "center", width: SIZE + 24 },
  ring: { backgroundColor: colors.danger, borderRadius: SIZE / 2, height: SIZE, position: "absolute", width: SIZE },
  button: { alignItems: "center", backgroundColor: colors.accent, borderRadius: SIZE / 2, height: SIZE, justifyContent: "center", width: SIZE },
  buttonRecording: { backgroundColor: colors.danger },
  buttonDisabled: { opacity: 0.4 },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  hint: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, paddingHorizontal: 24, textAlign: "center" },
});
