import { useCallback, useRef } from "react";
import { Pressable, StyleSheet, Text, View, type LayoutChangeEvent } from "react-native";

import { PauseIcon, PlayIcon, StopIcon } from "../../ui/components/MediaIcons";
import { colors, radii, typeScale } from "../../ui/tokens";
import { formatClock } from "./format";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import type { VoiceMessage } from "./voiceStore";

/** Swipe up/down on the seek bar with a screen reader moves by this much. */
const SEEK_STEP_MS = 5_000;

/**
 * The Telegram-shaped voice message on the newest agent reply (design.md
 * §9.11): play/pause, a thin position bar that is also the seek target, the
 * clock, the unplayed dot, and Stop. Subscribes to `playback` itself so 4 Hz
 * position ticks re-render this row and nothing above it.
 */
export function ReplyPlayer({ message, controller }: { message: VoiceMessage; controller: VoiceController }) {
  const playback = useVoice((s) => (s.playback?.messageId === message.id ? s.playback : undefined));
  const width = useRef(0);
  const onLayout = useCallback((event: LayoutChangeEvent) => {
    width.current = event.nativeEvent.layout.width;
  }, []);
  const playing = playback?.state === "playing";
  const loaded = playback !== undefined && playback.state !== "stopped";
  const durationMs = playback?.durationMs ?? 0;
  const positionMs = playback?.positionMs ?? 0;
  const fraction = durationMs > 0 ? Math.min(1, positionMs / durationMs) : 0;

  const toggle = useCallback(() => {
    if (!playback) controller.play(message.id);
    else if (playback.state === "playing") controller.pause();
    else controller.resume();
  }, [controller, message.id, playback]);

  const seekTo = useCallback((target: number) => {
    // A tap on a reply that is not loaded yet starts it; the position is only
    // known once the player reports a duration.
    if (!playback) {
      controller.play(message.id);
      return;
    }
    controller.seek(target);
  }, [controller, message.id, playback]);

  const seekAt = useCallback((x: number) => {
    if (playback && (durationMs <= 0 || width.current <= 0)) return;
    seekTo(Math.round((Math.min(Math.max(x, 0), width.current) / Math.max(width.current, 1)) * durationMs));
  }, [durationMs, playback, seekTo]);

  if (message.audioError) {
    return (
      <View style={styles.row}>
        <Text style={styles.unavailable}>Audio unavailable</Text>
        <Pressable accessibilityLabel="Retry synthesizing this reply" accessibilityRole="button" onPress={() => void controller.retrySpeak(message.id)} style={styles.retry}>
          <Text style={styles.retryLabel}>Retry</Text>
        </Pressable>
      </View>
    );
  }
  if (!message.fileUri) return null;

  return (
    <View style={styles.row}>
      <Pressable
        accessibilityLabel={playing ? "Pause reply" : message.played ? "Play reply" : "Play reply, not yet played"}
        accessibilityRole="button"
        onPress={toggle}
        style={({ pressed }) => [styles.playButton, pressed && styles.pressed]}
      >
        {playing ? <PauseIcon color={colors.accentInk} size={24} /> : <PlayIcon color={colors.accentInk} size={24} />}
      </Pressable>
      <View style={styles.trackColumn}>
        <Pressable
          accessibilityActions={[{ name: "increment", label: "Skip forward" }, { name: "decrement", label: "Skip back" }]}
          accessibilityLabel="Playback position"
          accessibilityRole="adjustable"
          accessibilityValue={{ min: 0, max: durationMs, now: positionMs, text: `${formatClock(positionMs)} of ${formatClock(durationMs)}` }}
          onAccessibilityAction={(event) => seekTo(event.nativeEvent.actionName === "increment" ? positionMs + SEEK_STEP_MS : positionMs - SEEK_STEP_MS)}
          onLayout={onLayout}
          onPress={(event) => seekAt(event.nativeEvent.locationX)}
          style={styles.trackHit}
        >
          <View style={styles.track}>
            <View style={[styles.progress, { width: `${fraction * 100}%` }]} />
          </View>
        </Pressable>
        <View style={styles.clockRow}>
          <Text style={styles.clock}>{formatClock(playback ? positionMs : 0)}{durationMs > 0 ? ` / ${formatClock(durationMs)}` : ""}</Text>
          {message.played ? null : <View importantForAccessibility="no" style={styles.unplayedDot} />}
        </View>
      </View>
      {/* The slot is always reserved so the track does not shrink under the finger when Play is tapped. */}
      <Pressable
        accessibilityElementsHidden={!loaded}
        accessibilityLabel="Stop reply"
        accessibilityRole="button"
        disabled={!loaded}
        importantForAccessibility={loaded ? "auto" : "no-hide-descendants"}
        onPress={() => controller.stop()}
        style={({ pressed }) => [styles.stopButton, pressed && styles.pressed, !loaded && styles.stopHidden]}
      >
        <StopIcon color={colors.chromeInk} size={22} />
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  row: { alignItems: "center", flexDirection: "row", gap: 8, marginTop: 8 },
  playButton: { alignItems: "center", backgroundColor: colors.accent, borderRadius: 24, height: 48, justifyContent: "center", width: 48 },
  stopButton: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
  stopHidden: { opacity: 0 },
  pressed: { opacity: 0.75 },
  trackColumn: { flex: 1 },
  // Hit slop is clipped by the parent, so the target is the view itself: 48 dp tall around a 3 dp rail.
  trackHit: { justifyContent: "center", minHeight: 48 },
  track: { backgroundColor: colors.chromeFaint, borderRadius: 2, height: 3, overflow: "hidden" },
  progress: { backgroundColor: colors.accent, height: 3 },
  clockRow: { alignItems: "center", flexDirection: "row", gap: 6 },
  clock: { color: colors.chromeDim, fontSize: typeScale.meta },
  unplayedDot: { backgroundColor: colors.accent, borderRadius: 4, height: 8, width: 8 },
  unavailable: { color: colors.chromeDim, flex: 1, fontSize: typeScale.rowSecondary },
  retry: { alignItems: "center", borderRadius: radii.card, height: 48, justifyContent: "center", paddingHorizontal: 12 },
  retryLabel: { color: colors.accent, fontSize: typeScale.body, fontWeight: "600" },
});
