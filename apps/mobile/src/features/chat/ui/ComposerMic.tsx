import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Animated, type GestureResponderEvent, Pressable, StyleSheet, Text, View } from "react-native";

import { ArrowUpIcon, LockIcon, MicIcon, TrashIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { horizontalVoiceGesture, previewVoiceGesture, type VoiceGestureDirection } from "../../voice/voiceGesture";
import type { VoicePhase } from "../../voice/voiceStore";

/** A press shorter than this is a tap: voice mode opens and nothing is recorded. */
export const DICTATION_HOLD_MS = 250;
/**
 * How far a swipe goes to lock or discard. The reply field's row is narrower
 * than the talk pane: its Discard sits about this far right of the
 * mic, by the screen's edge, so reaching it is the swipe.
 */
export const DICTATION_SWIPE = 40;
/** How far the mic follows the thumb toward Lock or Discard. */
const DRAG_LIMIT = DICTATION_SWIPE - 8;

/** Dictating one message from the reply field's mic (mobile-voice-mock.html). */
export interface ComposerDictation {
  phase: VoicePhase;
  /** Why a hold records nothing now; empty when it can. */
  refusal: string;
  /** Shows the "Hold to dictate" tip above the mic. */
  tip: boolean;
  /** The finger went down on the mic: the microphone gets ready. */
  onPressIn: () => void;
  /** The finger came up, whatever the press did. */
  onRelease: () => void;
  /** Held past the tap: recording starts. */
  onBegin: () => void;
  /** Transcribe the take and send it. */
  onSubmit: () => void;
  onLock: () => void;
  onCancel: () => void;
  /** A hold that records nothing says why. */
  onRefused: (reason: string) => void;
}

export interface ComposerMicProps {
  /** Voice mode is on, and voice is not ready for the talk pane: the mic shows it, and a tap turns it off. */
  on: boolean;
  /** A tap: voice mode on, or off when `on`. */
  onTap: () => void;
  /** Absent offers the tap alone. */
  dictation: ComposerDictation | undefined;
  /** The reply field's own reason a message cannot go now; a hold says it. */
  sendRefusal: string;
  disabled: boolean;
}

/**
 * The reply field's mic, with the recording strip it lays over the field's
 * row while a take is on (rendered as the row's children, so the strip can
 * cover the row and the mic stays the element the finger is on). A tap opens
 * voice mode; a hold dictates: let go to send, swipe left to lock, right to
 * discard, as in the talk pane. A finger that leaves the mic mid-take
 * locks it, so the take is never left recording with nothing to end it.
 */
export function ComposerMic({ on, onTap, dictation, sendRefusal, disabled }: ComposerMicProps) {
  const phase = dictation?.phase ?? "idle";
  const recording = phase === "recording";
  const busy = phase === "canceling" || phase === "transcribing" || phase === "sending";
  // The timer reads the props when it fires, not when the finger went down.
  const latest = useRef({ dictation, sendRefusal, phase });
  latest.current = { dictation, sendRefusal, phase };
  const press = useRef<{ x: number; y: number; began: boolean; refused: boolean; action?: VoiceGestureDirection } | undefined>(undefined);
  const holdTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const dragX = useRef(new Animated.Value(0)).current;
  const [direction, setDirection] = useState<VoiceGestureDirection>();

  useEffect(() => () => clearTimeout(holdTimer.current), []);
  useEffect(() => {
    if (recording) return;
    dragX.setValue(0);
    setDirection(undefined);
  }, [dragX, recording]);

  const classify = (event: GestureResponderEvent): VoiceGestureDirection | undefined => {
    const current = press.current;
    if (!current?.began || current.action || latest.current.phase !== "recording") return current?.action;
    const dx = event.nativeEvent.pageX - current.x;
    const dy = event.nativeEvent.pageY - current.y;
    const preview = previewVoiceGesture(dx, dy);
    dragX.setValue(preview ? Math.max(-DRAG_LIMIT, Math.min(DRAG_LIMIT, dx)) : 0);
    setDirection(preview);
    const action = horizontalVoiceGesture(dx, dy, DICTATION_SWIPE);
    if (action === "lock") latest.current.dictation?.onLock();
    else if (action === "cancel") latest.current.dictation?.onCancel();
    if (action) current.action = action;
    return action;
  };

  return (
    <>
      <Pressable
        accessibilityHint={on ? "Turns voice mode off" : dictation ? "Tap for voice mode. Hold to dictate a message." : "Turns voice mode on"}
        accessibilityLabel="Talk"
        accessibilityRole="button"
        accessibilityState={{ disabled: disabled || busy, selected: on }}
        disabled={disabled || busy}
        hitSlop={6}
        onPress={() => {
          // Not fired when the press is taken away (navigation, the app
          // leaving): only a real release opens voice mode or sends.
          // A screen reader's activation comes without a press: it is a tap.
          const current = press.current;
          press.current = undefined;
          if (current?.refused) return;
          if (!current?.began) onTap();
          else if (!current.action) latest.current.dictation?.onSubmit();
        }}
        onPressIn={(event) => {
          // The finger coming back onto the mic during its own take is the same press.
          if (press.current?.began && latest.current.phase !== "idle") return;
          press.current = { x: event.nativeEvent.pageX, y: event.nativeEvent.pageY, began: false, refused: false };
          if (!dictation) return;
          // A hold that will be refused does not wake the microphone (or ask for it).
          if (!dictation.refusal && !sendRefusal) dictation.onPressIn();
          holdTimer.current = setTimeout(() => {
            holdTimer.current = undefined;
            const current = press.current;
            const now = latest.current;
            if (!current || !now.dictation) return;
            const refusal = now.dictation.refusal || now.sendRefusal;
            if (refusal) {
              current.refused = true;
              now.dictation.onRefused(refusal);
              return;
            }
            current.began = true;
            now.dictation.onBegin();
          }, DICTATION_HOLD_MS);
        }}
        onPressOut={(event) => {
          clearTimeout(holdTimer.current);
          holdTimer.current = undefined;
          // Let go, or slid off the mic: a take still on is locked. A real
          // release sends it next (onPress); otherwise it waits on the strip's
          // Discard and Send instead of recording on with no finger.
          const action = classify(event);
          if (press.current?.began && !action && latest.current.phase !== "recordingLocked") latest.current.dictation?.onLock();
          latest.current.dictation?.onRelease();
        }}
        onTouchMove={classify}
        // A thumb on its way to Discard, past the screen's edge, has not let go.
        pressRetentionOffset={160}
        style={({ pressed }) => [styles.mic, recording && styles.micAbove, pressed && !recording && styles.pressed]}
      >
        <Animated.View style={[styles.disc, on && styles.discOn, recording && styles.discRecording, { transform: [{ translateX: dragX }, { scale: recording ? 1.25 : 1 }] }]}>
          <MicIcon color={recording ? colors.chromeInkStrong : on ? colors.accent : colors.chromeDim} size={18} />
        </Animated.View>
      </Pressable>
      {dictation && phase !== "idle" ? <DictationStrip direction={direction} dictation={dictation} /> : null}
    </>
  );
}

/** Says what the mic does until it has been held once. Drawn above the reply field, outside its clipped box. */
export function MicTip() {
  return (
    <View accessibilityElementsHidden importantForAccessibility="no-hide-descendants" pointerEvents="none" style={styles.tip}>
      <Text style={styles.tipTitle}>Hold to dictate a message</Text>
      <Text style={styles.tipLine}>Tap for voice mode</Text>
    </View>
  );
}

/** Laid over the reply field's row while a take is on. */
function DictationStrip({ dictation, direction }: { dictation: ComposerDictation; direction: VoiceGestureDirection | undefined }) {
  const { phase } = dictation;
  const recording = phase === "recording";
  const locked = phase === "recordingLocked";
  const [startedAt] = useState(Date.now);
  const [now, setNow] = useState(startedAt);
  useEffect(() => {
    if (!recording && !locked) return undefined;
    const tick = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(tick);
  }, [locked, recording]);
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000));
  const elapsed = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;

  if (!recording && !locked) {
    const label = phase === "canceling" ? "Discarding…" : phase === "transcribing" ? "Transcribing…" : "Sending…";
    return (
      <View style={[styles.strip, styles.stripPlain]}>
        <ActivityIndicator color={colors.chromeDim} size="small" />
        <Text accessibilityLiveRegion="polite" style={styles.stripLabel}>{label}</Text>
      </View>
    );
  }
  if (locked) {
    return (
      <View style={[styles.strip, styles.stripPlain]}>
        <Pressable accessibilityLabel="Discard the recording" accessibilityRole="button" hitSlop={6} onPress={dictation.onCancel} style={({ pressed }) => [styles.target, styles.discard, pressed && styles.pressed]}>
          <TrashIcon color={colors.dangerInk} size={16} />
        </Pressable>
        <View style={styles.stripMiddle}>
          <View style={styles.dot} />
          <Text {...fixedChromeText} style={styles.elapsed}>{elapsed}</Text>
          <Text accessibilityLiveRegion="polite" style={styles.stripLabel}>Locked</Text>
        </View>
        <Pressable accessibilityLabel="Send the recording" accessibilityRole="button" hitSlop={6} onPress={dictation.onSubmit} style={({ pressed }) => [styles.send, pressed && styles.pressed]}>
          <ArrowUpIcon color={colors.accentInk} size={18} />
        </Pressable>
      </View>
    );
  }
  // The finger is on the mic, which stays above the strip; the strip only shows where to swipe.
  return (
    <View pointerEvents="none" style={[styles.strip, styles.stripRecording]}>
      <View style={[styles.target, styles.lock, direction === "lock" && styles.lockActive]}>
        <LockIcon color={direction === "lock" ? colors.accentInk : colors.accent} size={15} />
      </View>
      <View style={styles.stripMiddle}>
        <View style={styles.dot} />
        <Text {...fixedChromeText} style={styles.elapsed}>{elapsed}</Text>
        <Text accessibilityLiveRegion="polite" numberOfLines={1} style={styles.stripLabel}>‹ Lock · Discard ›</Text>
      </View>
      <View style={[styles.target, styles.discard, styles.discardEnd, direction === "cancel" && styles.discardActive]}>
        <TrashIcon color={direction === "cancel" ? colors.chromeInkStrong : colors.dangerInk} size={15} />
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  mic: { alignItems: "center", height: 34, justifyContent: "center", width: 34 },
  /** Over the strip, which is drawn after it. */
  micAbove: { zIndex: 2 },
  disc: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 17, borderWidth: 1, height: 34, justifyContent: "center", width: 34 },
  discOn: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  discRecording: { backgroundColor: colors.danger, borderColor: colors.danger },
  pressed: { opacity: 0.75 },
  /** Over the mic: right of the field, above its box. */
  tip: { backgroundColor: colors.chromeRaised, borderColor: colors.accent, borderRadius: 9, borderWidth: 1, bottom: "100%", marginBottom: 4, paddingHorizontal: 10, paddingVertical: 6, position: "absolute", right: 40 },
  tipTitle: { color: colors.chromeInkStrong, fontSize: typeScale.meta, fontWeight: "600" },
  tipLine: { color: colors.chromeDim, fontSize: typeScale.meta },
  strip: { alignItems: "center", bottom: 0, flexDirection: "row", gap: 8, left: 0, paddingHorizontal: 8, position: "absolute", right: 0, top: 0, zIndex: 1 },
  stripRecording: { backgroundColor: colors.dangerWash },
  stripPlain: { backgroundColor: colors.chromeBg },
  stripMiddle: { alignItems: "center", flex: 1, flexDirection: "row", gap: 6, minWidth: 0 },
  stripLabel: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary },
  dot: { backgroundColor: colors.danger, borderRadius: 4, height: 8, width: 8 },
  elapsed: { color: colors.chromeInkStrong, fontSize: typeScale.rowSecondary, fontVariant: ["tabular-nums"], fontWeight: "600" },
  target: { alignItems: "center", borderRadius: 17, borderWidth: 1, height: 34, justifyContent: "center", width: 34 },
  lock: { borderColor: colors.accent },
  lockActive: { backgroundColor: colors.accent },
  discard: { borderColor: colors.danger },
  /** Clear of the mic, which sits just left of it. */
  discardEnd: { marginLeft: 42 },
  discardActive: { backgroundColor: colors.danger },
  send: { alignItems: "center", backgroundColor: colors.accent, borderRadius: 9, height: 34, justifyContent: "center", width: 34 },
});
