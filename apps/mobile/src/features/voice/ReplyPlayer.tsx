import { useCallback, useRef } from "react";
import { Pressable, StyleSheet, Text, View, type LayoutChangeEvent } from "react-native";

import { colors, radii, typeScale } from "../../ui/tokens";
import { formatClock } from "./format";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import type { VoiceMessage } from "./voiceStore";

/**
 * The Telegram-shaped voice message on the newest agent reply (design.md
 * §9.11): play/pause, a thin position bar that is also the seek target, the
 * clock, and the unplayed dot. Subscribes to `playback` itself so 4 Hz
 * position ticks re-render this row and nothing above it.
 */
export function ReplyPlayer({ message, controller }: { message: VoiceMessage; controller: VoiceController }) {
  const playback = useVoice((s) => (s.playback?.messageId === message.id ? s.playback : undefined));
  const width = useRef(0);
  const onLayout = useCallback((event: LayoutChangeEvent) => {
    width.current = event.nativeEvent.layout.width;
  }, []);
  const playing = playback?.state === "playing";
  const durationMs = playback?.durationMs ?? 0;
  const positionMs = playback?.positionMs ?? 0;
  const fraction = durationMs > 0 ? Math.min(1, positionMs / durationMs) : 0;

  const toggle = useCallback(() => {
    if (!playback) controller.play(message.id);
    else if (playback.state === "playing") controller.pause();
    else controller.resume();
  }, [controller, message.id, playback]);

  const seek = useCallback((x: number) => {
    if (!playback || durationMs <= 0 || width.current <= 0) return;
    controller.seek(Math.round((Math.min(Math.max(x, 0), width.current) / width.current) * durationMs));
  }, [controller, durationMs, playback]);

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
        accessibilityLabel={playing ? "Pause reply" : "Play reply"}
        accessibilityRole="button"
        onPress={toggle}
        style={({ pressed }) => [styles.playButton, pressed && styles.pressed]}
      >
        <Text style={styles.playGlyph}>{playing ? "❙❙" : "▶"}</Text>
      </Pressable>
      <View style={styles.trackColumn}>
        <Pressable
          accessibilityLabel="Seek"
          accessibilityRole="adjustable"
          accessibilityValue={{ min: 0, max: durationMs, now: positionMs }}
          hitSlop={{ top: 14, bottom: 14 }}
          onLayout={onLayout}
          onPress={(event) => seek(event.nativeEvent.locationX)}
          style={styles.trackHit}
        >
          <View style={styles.track}>
            <View style={[styles.progress, { width: `${fraction * 100}%` }]} />
          </View>
        </Pressable>
        <View style={styles.clockRow}>
          <Text style={styles.clock}>{formatClock(playback ? positionMs : 0)}{durationMs > 0 ? ` / ${formatClock(durationMs)}` : ""}</Text>
          {message.played ? null : <View accessibilityLabel="Unplayed" style={styles.unplayedDot} />}
        </View>
      </View>
      {playback && playback.state !== "stopped" ? (
        <Pressable accessibilityLabel="Stop reply" accessibilityRole="button" onPress={() => controller.stop()} style={({ pressed }) => [styles.stopButton, pressed && styles.pressed]}>
          <Text style={styles.stopGlyph}>■</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  row: { alignItems: "center", flexDirection: "row", gap: 8, marginTop: 8 },
  playButton: { alignItems: "center", backgroundColor: colors.accent, borderRadius: 24, height: 48, justifyContent: "center", width: 48 },
  playGlyph: { color: colors.accentInk, fontSize: 18, fontWeight: "600" },
  stopButton: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
  stopGlyph: { color: colors.chromeInk, fontSize: 18 },
  pressed: { opacity: 0.75 },
  trackColumn: { flex: 1, gap: 4 },
  trackHit: { justifyContent: "center", minHeight: 20 },
  track: { backgroundColor: colors.chromeBorder, borderRadius: 2, height: 3, overflow: "hidden" },
  progress: { backgroundColor: colors.accent, height: 3 },
  clockRow: { alignItems: "center", flexDirection: "row", gap: 6 },
  clock: { color: colors.chromeDim, fontSize: typeScale.meta },
  unplayedDot: { backgroundColor: colors.accent, borderRadius: 4, height: 8, width: 8 },
  unavailable: { color: colors.chromeDim, flex: 1, fontSize: typeScale.rowSecondary },
  retry: { alignItems: "center", borderRadius: radii.card, height: 48, justifyContent: "center", paddingHorizontal: 12 },
  retryLabel: { color: colors.accent, fontSize: typeScale.body, fontWeight: "600" },
});
