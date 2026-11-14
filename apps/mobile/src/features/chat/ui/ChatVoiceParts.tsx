import { Pressable, StyleSheet, Text, View } from "react-native";

import { prefsStore, type VoicePlaybackRate } from "../../../store/prefsStore";
import { colors, typeScale } from "../../../ui/tokens";
import { MicButton } from "../../voice/MicButton";
import { ReplyPlayer } from "../../voice/ReplyPlayer";
import { SpeedButton } from "../../voice/SpeedButton";
import type { VoiceController } from "../../voice/VoiceController";
import type { VoiceMessage, VoicePhase } from "../../voice/voiceStore";

/** The newest reply's audio, as a row after the conversation: play, position, speed. */
export function ChatReplyRow({ controller, message, playbackRate }: { controller: VoiceController; message: VoiceMessage; playbackRate: VoicePlaybackRate }) {
  return (
    <View accessibilityLabel="Reply read aloud" style={styles.reply}>
      <View style={styles.player}>
        <ReplyPlayer controller={controller} message={message} />
      </View>
      <SpeedButton onChange={(rate) => prefsStore.getState().setVoicePlaybackRate(rate)} rate={playbackRate} />
    </View>
  );
}

/** The hold-to-talk bar that replaces the composer. A locked take is sent by a tap anywhere on it. */
export function ChatVoiceBar({ controller, phase, hint, working, onStop }: { controller: VoiceController; phase: VoicePhase; hint: string; working: boolean; onStop: () => void }) {
  const locked = phase === "recordingLocked";
  return (
    <View>
      {working ? (
        <Pressable accessibilityLabel="Stop the agent" accessibilityRole="button" onPress={onStop} style={styles.stop}>
          <Text style={styles.stopLabel}>Stop</Text>
        </Pressable>
      ) : null}
      <View>
        <MicButton
          disabled={hint !== ""}
          hint={hint}
          onCancel={() => void controller.cancelUtterance()}
          onLock={() => controller.lockUtterance()}
          onPressIn={() => controller.beginUtterance()}
          onSubmit={() => void controller.endUtterance()}
          phase={phase}
          variant="bar"
        />
        {locked ? (
          <Pressable
            accessibilityHint="Stops recording and sends it"
            accessibilityLabel="Stop and send locked recording"
            accessibilityRole="button"
            onPressIn={() => void controller.endUtterance()}
            style={StyleSheet.absoluteFill}
          />
        ) : null}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  /** The mock's "play · Reply · 1.5×" row: the reused player, then the speed. ReplyPlayer brings its own top margin. */
  reply: { alignItems: "center", borderColor: colors.chromeHairline, borderRadius: 7, borderWidth: 1, flexDirection: "row", gap: 4, paddingBottom: 4, paddingHorizontal: 8 },
  player: { flex: 1, marginTop: -4 },
  stop: { alignItems: "center", alignSelf: "flex-end", borderColor: colors.danger, borderRadius: 999, borderWidth: 1, justifyContent: "center", marginBottom: 6, marginRight: 10, minHeight: 44, paddingHorizontal: 18 },
  stopLabel: { color: colors.dangerInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
});
