import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Animated, Easing, type GestureResponderEvent, Pressable, StyleSheet, Text, View } from "react-native";

import { LockIcon, MicIcon, TrashIcon } from "../../ui/components/MediaIcons";
import { colors, typeScale } from "../../ui/tokens";
import { horizontalVoiceGesture, previewVoiceGesture, VOICE_GESTURE_THRESHOLD, type VoiceGestureDirection } from "./voiceGesture";
import type { VoicePhase } from "./voiceStore";

export interface MicButtonProps {
  phase: VoicePhase;
  disabled: boolean;
  /** Why it is disabled, shown under the button; empty when enabled. */
  hint: string;
  onPressIn: () => void;
  onSubmit: () => void;
  onLock: () => void;
  onCancel: () => void;
}

const SIZE = 96;
const DRAG_PREVIEW_LIMIT = VOICE_GESTURE_THRESHOLD - 8;

/**
 * The press-and-hold surface (design.md §9.11). The whole pane it fills is the
 * target: a thumb anywhere below the controls row starts recording, so the
 * screen works without looking at it. The disc is the visual anchor (pulsing
 * ring while recording, spinner while the host works), not the hit area.
 */
export function MicButton({ phase, disabled, hint, onPressIn, onSubmit, onLock, onCancel }: MicButtonProps) {
  const pulse = useRef(new Animated.Value(0)).current;
  const dragX = useRef(new Animated.Value(0)).current;
  const [gestureDirection, setGestureDirection] = useState<VoiceGestureDirection>();
  const recording = phase === "recording" || phase === "recordingLocked";
  const locked = phase === "recordingLocked";
  const busy = phase === "canceling" || phase === "transcribing" || phase === "sending";
  const gesture = useRef<{ x: number; y: number; action?: "lock" | "cancel" } | undefined>(undefined);
  const completedAction = useRef<"lock" | "cancel" | undefined>(undefined);

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

  useEffect(() => {
    if (recording && !locked) return;
    dragX.setValue(0);
    setGestureDirection(undefined);
  }, [dragX, locked, recording]);

  const ringStyle = {
    opacity: pulse.interpolate({ inputRange: [0, 1], outputRange: [0.6, 0] }),
    transform: [{ scale: pulse.interpolate({ inputRange: [0, 1], outputRange: [1, 1.6] }) }],
  };
  const label = locked ? "Locked · tap anywhere to send" : recording ? "Listening…" : phase === "canceling" ? "Canceling…" : phase === "transcribing" ? "Transcribing…" : phase === "sending" ? "Sending…" : "Hold anywhere here to talk";

  const resetGesturePreview = () => {
    dragX.setValue(0);
    setGestureDirection(undefined);
  };

  const classify = (event: GestureResponderEvent): "lock" | "cancel" | undefined => {
    const current = gesture.current;
    if (!current || current.action) return current?.action;
    const dx = event.nativeEvent.pageX - current.x;
    const dy = event.nativeEvent.pageY - current.y;
    const direction = previewVoiceGesture(dx, dy);
    dragX.setValue(direction ? Math.max(-DRAG_PREVIEW_LIMIT, Math.min(DRAG_PREVIEW_LIMIT, dx)) : 0);
    setGestureDirection(direction);
    const action = horizontalVoiceGesture(dx, dy);
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
      onPress={() => {
        // Unlike onPressOut, onPress is not fired when the native responder is
        // terminated by navigation or backgrounding. Only a genuine release
        // may submit, and a completed horizontal gesture already handled itself.
        const action = completedAction.current;
        completedAction.current = undefined;
        if (!action) onSubmit();
      }}
      onPressIn={(event) => {
        completedAction.current = undefined;
        gesture.current = { x: event.nativeEvent.pageX, y: event.nativeEvent.pageY };
        resetGesturePreview();
        onPressIn();
      }}
      onPressOut={(event) => {
        const action = classify(event);
        completedAction.current = action;
        gesture.current = undefined;
        if (!action) resetGesturePreview();
      }}
      onTouchMove={classify}
      // A thumb drifting well outside the pane mid-sentence must not count as a release.
      pressRetentionOffset={160}
      style={({ pressed }) => [styles.root, pressed && !disabled && !busy && styles.rootPressed]}
    >
      <View style={styles.stage}>
        {phase === "recording" ? (
          <>
            <GestureTarget active={gestureDirection === "lock"} action="lock" />
            <GestureTarget active={gestureDirection === "cancel"} action="cancel" />
          </>
        ) : null}
        <Animated.View style={[styles.micAnchor, { transform: [{ translateX: dragX }] }]}>
          {recording ? <Animated.View pointerEvents="none" style={[styles.ring, ringStyle]} /> : null}
          <View style={[styles.disc, recording && styles.discRecording, disabled && styles.discDisabled]}>
            {busy ? <ActivityIndicator color={colors.accentInk} size="large" /> : <MicIcon color={colors.accentInk} size={44} />}
          </View>
          {locked ? (
            <View style={styles.lockBadge}>
              <LockIcon color={colors.accentInk} size={15} />
            </View>
          ) : null}
        </Animated.View>
      </View>
      <Text accessibilityLiveRegion="polite" style={styles.label}>{label}</Text>
      {phase === "recording" ? <Text style={styles.gestureHint}>Swipe left to lock · right to discard</Text> : null}
      {disabled && hint ? <Text style={styles.hint}>{hint}</Text> : null}
    </Pressable>
  );
}

function GestureTarget({ action, active }: { action: VoiceGestureDirection; active: boolean }) {
  const lock = action === "lock";
  const color = active ? colors.accentInk : lock ? colors.accent : colors.dangerInk;
  return (
    <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" pointerEvents="none" style={[styles.gestureTarget, lock ? styles.lockTarget : styles.cancelTarget]}>
      <View style={[styles.targetDisc, lock ? styles.lockTargetDisc : styles.cancelTargetDisc, active && (lock ? styles.lockTargetActive : styles.cancelTargetActive)]}>
        {lock ? <LockIcon color={color} size={25} /> : <TrashIcon color={color} size={25} />}
      </View>
      <Text style={[styles.targetLabel, lock ? styles.lockTargetLabel : styles.cancelTargetLabel]}>{lock ? "Lock" : "Discard"}</Text>
    </View>
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
  stage: { alignItems: "center", alignSelf: "stretch", height: SIZE + 24, justifyContent: "center" },
  micAnchor: { alignItems: "center", height: SIZE + 24, justifyContent: "center", width: SIZE + 24, zIndex: 1 },
  ring: { backgroundColor: colors.danger, borderRadius: SIZE / 2, height: SIZE, position: "absolute", width: SIZE },
  disc: { alignItems: "center", backgroundColor: colors.accent, borderRadius: SIZE / 2, height: SIZE, justifyContent: "center", width: SIZE },
  discRecording: { backgroundColor: colors.danger },
  discDisabled: { opacity: 0.4 },
  lockBadge: { alignItems: "center", backgroundColor: colors.accent, borderColor: colors.chromeRaised, borderRadius: 16, borderWidth: 3, height: 32, justifyContent: "center", position: "absolute", right: 2, top: 2, width: 32 },
  gestureTarget: { alignItems: "center", gap: 6, position: "absolute", top: 21, width: 64 },
  lockTarget: { left: 16 },
  cancelTarget: { right: 16 },
  targetDisc: { alignItems: "center", backgroundColor: colors.chromeBg, borderRadius: 27, borderWidth: 1, height: 54, justifyContent: "center", width: 54 },
  lockTargetDisc: { borderColor: colors.accent },
  cancelTargetDisc: { borderColor: colors.danger },
  lockTargetActive: { backgroundColor: colors.accent },
  cancelTargetActive: { backgroundColor: colors.danger },
  targetLabel: { fontSize: 10, fontWeight: "700", letterSpacing: 0.8, textTransform: "uppercase" },
  lockTargetLabel: { color: colors.accent },
  cancelTargetLabel: { color: colors.dangerInk },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  gestureHint: { color: colors.chromeDim, fontSize: typeScale.meta, textAlign: "center" },
  hint: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, paddingHorizontal: 24, textAlign: "center" },
});
