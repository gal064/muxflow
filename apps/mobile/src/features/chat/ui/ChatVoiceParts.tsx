import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, fixedChromeText, radii, typeScale } from "../../../ui/tokens";
import { MicButton } from "../../voice/MicButton";
import { ReplyPlayer } from "../../voice/ReplyPlayer";
import type { VoiceController } from "../../voice/VoiceController";
import { VoiceSettingsControls } from "../../voice/VoiceSettingsControls";
import type { VoiceMessage, VoicePhase } from "../../voice/voiceStore";
import { KeyboardIcon } from "./HeaderIcons";

/** The newest reply's audio, as a row after the conversation. Its speed is the pane's. */
export function ChatReplyRow({ controller, message }: { controller: VoiceController; message: VoiceMessage }) {
  return (
    <View accessibilityLabel="Reply read aloud" style={styles.reply}>
      <ReplyPlayer controller={controller} message={message} />
    </View>
  );
}

export interface ChatVoicePaneProps {
  controller: VoiceController;
  phase: VoicePhase;
  /** Why the microphone is off; empty when it is on. */
  hint: string;
  working: boolean;
  /** Stop was pressed and the turn has not stopped yet. */
  stopping: boolean;
  onStop: () => void;
  /** Leaves voice mode for the reply field. */
  onKeyboard: () => void;
  /** Shows the card on dictating from the reply field, until Got it or a first dictation. */
  tip: boolean;
  onTipDone: () => void;
}

/** Under the hold label, always: the reply field's mic dictates one message without voice mode. */
export const DICTATION_NOTE = "Just one message? Hold the mic in the reply field.";

/**
 * Voice mode's talk pane in the reply field's place (mobile-voice-mock.html):
 * Speed and Autoplay, Stop while the agent works, and the way back to the
 * keyboard, over the Voice screen's hold surface. A locked take is sent by a
 * tap anywhere on the surface. Voice mode is where the reply field's other
 * gesture, a hold to dictate, is taught (mic-tip-mock.html): a card until
 * Got it or a first dictation, and a line under the hold label always.
 */
export function ChatVoicePane({ controller, phase, hint, working, stopping, onStop, onKeyboard, tip, onTipDone }: ChatVoicePaneProps) {
  const locked = phase === "recordingLocked";
  return (
    <View style={styles.pane}>
      <View accessibilityElementsHidden={locked} importantForAccessibility={locked ? "no-hide-descendants" : "auto"} style={styles.controls}>
        <VoiceSettingsControls />
        <View style={styles.spacer} />
        {working ? (
          <Pressable
            accessibilityLabel="Stop the agent"
            accessibilityRole="button"
            accessibilityState={{ disabled: stopping }}
            disabled={stopping}
            hitSlop={{ top: 6, bottom: 6 }}
            onPress={onStop}
            style={({ pressed }) => [styles.stop, stopping && styles.disabled, pressed && styles.pressed]}
          >
            <Text {...fixedChromeText} numberOfLines={1} style={styles.stopLabel}>Stop</Text>
          </Pressable>
        ) : null}
        <Pressable
          accessibilityHint="Leaves voice mode for the reply field"
          accessibilityLabel="Keyboard"
          accessibilityRole="button"
          // Not while a take is on: it is voice mode's, and leaving would send it as a dictation.
          disabled={phase !== "idle"}
          hitSlop={{ top: 6, bottom: 6 }}
          onPress={onKeyboard}
          style={({ pressed }) => [styles.keyboard, phase !== "idle" && styles.disabled, pressed && styles.pressed]}
        >
          <KeyboardIcon color={colors.chromeInk} size={20} />
        </Pressable>
      </View>
      {/* Kept while a take is locked, as the row above is: a pane that changed height would resize the terminal over it. */}
      {tip && hint === "" ? (
        <View accessibilityElementsHidden={locked} importantForAccessibility={locked ? "no-hide-descendants" : "auto"} style={styles.tip}>
          <Text style={styles.tipTitle}>Tip: dictate without voice mode</Text>
          <Text style={styles.tipText}>Hold the mic in the reply field to dictate one message. It's sent when you let go.</Text>
          <Pressable accessibilityRole="button" hitSlop={6} onPress={onTipDone} style={({ pressed }) => [styles.tipDone, pressed && styles.pressed]}>
            <Text {...fixedChromeText} style={styles.tipDoneLabel}>Got it</Text>
          </Pressable>
        </View>
      ) : null}
      <View>
        <MicButton
          disabled={hint !== ""}
          hint={hint}
          note={DICTATION_NOTE}
          onCancel={() => void controller.cancelUtterance()}
          onLock={() => controller.lockUtterance()}
          onPressIn={() => controller.beginUtterance()}
          onSubmit={() => void controller.endUtterance()}
          phase={phase}
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
  reply: { borderColor: colors.chromeHairline, borderRadius: 7, borderWidth: 1, paddingBottom: 4, paddingHorizontal: 8 },
  /** As the Voice screen's pane: a hairline above, the controls row, then the hold surface. */
  pane: { borderTopColor: colors.chromeBorder, borderTopWidth: 1 },
  controls: { alignItems: "center", flexDirection: "row", gap: 8, paddingBottom: 8, paddingHorizontal: 12, paddingTop: 8 },
  spacer: { flex: 1 },
  stop: { alignItems: "center", borderColor: colors.danger, borderRadius: radii.card, borderWidth: 1, height: 40, justifyContent: "center", paddingHorizontal: 14 },
  stopLabel: { color: colors.dangerInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  keyboard: { alignItems: "center", backgroundColor: colors.chromeRaised, borderRadius: radii.card, height: 40, justifyContent: "center", width: 44 },
  tip: { backgroundColor: colors.chromeHairline, borderColor: colors.chromeBorder, borderRadius: radii.card, borderWidth: 1, gap: 2, marginBottom: 10, marginHorizontal: 12, paddingBottom: 8, paddingHorizontal: 12, paddingTop: 10 },
  tipTitle: { color: colors.chromeInkStrong, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  tipText: { color: colors.chromeInk, fontSize: typeScale.meta },
  tipDone: { alignItems: "center", alignSelf: "flex-end", backgroundColor: colors.accent, borderRadius: radii.card, height: 36, justifyContent: "center", marginTop: 6, paddingHorizontal: 14 },
  tipDoneLabel: { color: colors.accentInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  disabled: { opacity: 0.4 },
  pressed: { opacity: 0.75 },
});
