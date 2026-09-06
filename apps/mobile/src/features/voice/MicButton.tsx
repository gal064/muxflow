import { useEffect, useRef } from "react";
import { ActivityIndicator, Animated, Easing, type GestureResponderEvent, Pressable, StyleSheet, Text, View } from "react-native";

import { MicIcon } from "../../ui/components/MediaIcons";
import { colors, typeScale } from "../../ui/tokens";
import { horizontalVoiceGesture } from "./voiceGesture";
import type { VoicePhase } from "./voiceStore";

export interface MicButtonProps {
  phase: VoicePhase;
  disabled: boolean;
  /** Why it is disabled, shown under the button; empty when enabled. */
  hint: string;
  onPressIn: () => void;
  onPressOut: () => void;
  onLock: () => void;
  onCancel: () => void;
}

const SIZE = 96;

/**
 * The press-and-hold surface (design.md §9.11). The whole pane it fills is the
 * target: a thumb anywhere below the controls row starts recording, so the
 * screen works without looking at it. The disc is the visual anchor (pulsing
 * ring while recording, spinner while the host works), not the hit area.
 */
export function MicButton({ phase, disabled, hint, onPressIn, onPressOut, onLock, onCancel }: MicButtonProps) {
  const pulse = useRef(new Animated.Value(0)).current;
  const recording = phase === "recording" || phase === "recordingLocked";
  const locked = phase === "recordingLocked";
  const busy = phase === "canceling" || phase === "transcribing" || phase === "sending";
  const gesture = useRef<{ x: number; y: number; action?: "lock" | "cancel" } | undefined>(undefined);

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
  const label = locked ? "Locked · tap anywhere to send" : recording ? "Listening…" : phase === "canceling" ? "Canceling…" : phase === "transcribing" ? "Transcribing…" : phase === "sending" ? "Sending…" : "Hold anywhere here to talk";

  const classify = (event: GestureResponderEvent): "lock" | "cancel" | undefined => {
    const current = gesture.current;
    if (!current || current.action) return current?.action;
    const action = horizontalVoiceGesture(event.nativeEvent.pageX - current.x, event.nativeEvent.pageY - current.y);
    if (action === "lock") {
      current.action = action;
      onLock();
    } else if (action === "cancel") {
      current.action = action;
      onCancel();
    }
    return action;
  };

  return (
    <Pressable
      accessibilityHint="Hold and release to send, swipe left to lock, or swipe right to cancel"
      accessibilityLabel={label}
      accessibilityRole="button"
      accessibilityState={{ disabled: disabled || busy, busy }}
      disabled={disabled || busy}
      onPressIn={(event) => {
        gesture.current = { x: event.nativeEvent.pageX, y: event.nativeEvent.pageY };
        onPressIn();
      }}
      onPressOut={(event) => {
        const action = classify(event);
        gesture.current = undefined;
        if (!action) onPressOut();
      }}
      onTouchMove={classify}
      // A thumb drifting well outside the pane mid-sentence must not count as a release.
      pressRetentionOffset={160}
      style={({ pressed }) => [styles.root, pressed && !disabled && !busy && styles.rootPressed]}
    >
      <View style={styles.stage}>
        {recording ? <Animated.View pointerEvents="none" style={[styles.ring, ringStyle]} /> : null}
        <View style={[styles.disc, recording && styles.discRecording, disabled && styles.discDisabled]}>
          {busy ? <ActivityIndicator color={colors.accentInk} size="large" /> : <MicIcon color={colors.accentInk} size={44} />}
        </View>
      </View>
      <Text accessibilityLiveRegion="polite" style={styles.label}>{label}</Text>
      {disabled && hint ? <Text style={styles.hint}>{hint}</Text> : null}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  // Grow-only: in the pane's natural mode (no fixed height) the surface sizes to
  // its content; a `flex: 1` there means basis 0 and a clipped disc. In the
  // large mode the fixed-height pane hands it the remaining height.
  // One step off the page so "hold anywhere here" has a visible edge; the controls row above stays on the page.
  root: { alignItems: "center", alignSelf: "stretch", backgroundColor: colors.chromeRaised, flexGrow: 1, flexShrink: 0, gap: 8, justifyContent: "center", paddingBottom: 12, paddingTop: 12 },
  /** The whole slab shows the press, so a thumb at its edge still sees an answer. */
  rootPressed: { backgroundColor: colors.accentWash },
  stage: { alignItems: "center", height: SIZE + 24, justifyContent: "center", width: SIZE + 24 },
  ring: { backgroundColor: colors.danger, borderRadius: SIZE / 2, height: SIZE, position: "absolute", width: SIZE },
  disc: { alignItems: "center", backgroundColor: colors.accent, borderRadius: SIZE / 2, height: SIZE, justifyContent: "center", width: SIZE },
  discRecording: { backgroundColor: colors.danger },
  discDisabled: { opacity: 0.4 },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  hint: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, paddingHorizontal: 24, textAlign: "center" },
});
