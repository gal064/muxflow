import { Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";

import type { ChatCommand } from "../../../protocol/gen/chat_pb";
import { ArrowUpIcon, MicIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, fonts, typeScale } from "../../../ui/tokens";
import type { CommandGroup } from "../chatModel";

export interface ComposerProps {
  draft: string;
  onDraft: (text: string) => void;
  /** The slash list for the draft; empty hides it. */
  commands: CommandGroup[];
  onPickCommand: (command: ChatCommand) => void;
  /** The agent is on a turn: Stop shows, and Send only when it takes messages mid-turn. */
  working: boolean;
  canSendMidTurn: boolean;
  /** A request card waits for an answer; the agent takes no message until then unless it takes them mid-turn. */
  waiting: boolean;
  /** The draft is a `/btw` question: it goes to a fork, so the main turn does not hold it. */
  sideQuestion: boolean;
  /** Nothing can be sent (not connected, the chat ended). */
  disabled: boolean;
  sending: boolean;
  /** Set while an earlier message is being edited. */
  editing: boolean;
  onCancelEdit: () => void;
  onSend: () => void;
  onStop: () => void;
  /** Stop was pressed and the turn has not stopped yet: it is off until then. */
  stopping?: boolean;
  /** The field took the focus. */
  onFocus?: (() => void) | undefined;
  /** Turns voice mode on. Absent hides the button. */
  onMic?: (() => void) | undefined;
}

/** The reply field: text, the mic, and Send, which is Stop while the agent works. */
export function Composer(props: ComposerProps) {
  const { draft, onDraft, commands, onPickCommand, working, canSendMidTurn, waiting, sideQuestion, disabled, sending, editing, onCancelEdit, onSend, onStop, stopping = false, onFocus, onMic } = props;
  const hasText = draft.trim() !== "";
  const showSend = !working || ((canSendMidTurn || sideQuestion) && hasText) || editing;
  const blocked = waiting && !canSendMidTurn;
  const sendDisabled = disabled || sending || (blocked && !sideQuestion) || !hasText;
  const placeholder = blocked ? "Answer the card above" : working && canSendMidTurn ? "Send a follow-up while it works" : "Reply";
  return (
    <View style={styles.wrap}>
      {commands.length > 0 ? <SlashList groups={commands} onPick={onPickCommand} /> : null}
      <View style={[styles.box, disabled && styles.boxDisabled]}>
        {editing ? (
          <View style={styles.banner}>
            <Text style={styles.bannerText}>Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.</Text>
            <Pressable accessibilityRole="button" hitSlop={8} onPress={onCancelEdit} style={styles.bannerCancel}>
              <Text {...fixedChromeText} style={styles.bannerCancelLabel}>Cancel</Text>
            </Pressable>
          </View>
        ) : null}
        <View style={styles.row}>
          <TextInput
            accessibilityLabel="Message"
            editable={!disabled}
            multiline
            onChangeText={onDraft}
            onFocus={onFocus}
            placeholder={placeholder}
            placeholderTextColor={colors.chromeFaint}
            style={styles.input}
            value={draft}
          />
          {onMic ? (
            <Pressable accessibilityHint="Turns voice mode on" accessibilityLabel="Talk" accessibilityRole="button" disabled={disabled} hitSlop={6} onPress={onMic} style={({ pressed }) => [styles.mic, pressed && styles.pressed]}>
              <MicIcon color={colors.chromeDim} size={18} />
            </Pressable>
          ) : null}
          {working && !editing ? (
            <Pressable
              accessibilityLabel="Stop"
              accessibilityRole="button"
              accessibilityState={{ disabled: stopping }}
              disabled={stopping}
              hitSlop={6}
              onPress={onStop}
              style={({ pressed }) => [styles.send, styles.stop, stopping && styles.disabled, pressed && styles.pressed]}
            >
              <View style={styles.stopGlyph} />
            </Pressable>
          ) : null}
          {showSend ? (
            <Pressable
              accessibilityLabel="Send"
              accessibilityRole="button"
              accessibilityState={{ disabled: sendDisabled }}
              disabled={sendDisabled}
              hitSlop={6}
              onPress={onSend}
              // Nothing typed is a plain grey button; any other reason it is off dims it.
              style={({ pressed }) => [styles.send, hasText ? sendDisabled && styles.disabled : styles.sendEmpty, pressed && styles.pressed]}
            >
              <ArrowUpIcon color={hasText ? colors.accentInk : colors.chromeFaint} size={18} />
            </Pressable>
          ) : null}
        </View>
      </View>
    </View>
  );
}

function SlashList({ groups, onPick }: { groups: CommandGroup[]; onPick: (command: ChatCommand) => void }) {
  return (
    <View style={styles.pop}>
      <ScrollView keyboardShouldPersistTaps="always" style={styles.popScroll}>
        {groups.map((group) => (
          <View key={group.title}>
            <Text style={styles.section}>{group.title}</Text>
            {group.commands.map((command) => (
              <Pressable
                accessibilityLabel={`${group.sigil}${command.name}${command.description ? `, ${command.description}` : ""}`}
                accessibilityRole="button"
                key={`${command.kind}:${command.name}`}
                onPress={() => onPick(command)}
                style={({ pressed }) => [styles.entry, pressed && styles.entryPressed]}
              >
                <View style={styles.entryLine}>
                  <Text numberOfLines={1} style={styles.entryName}>{group.sigil}{command.name}</Text>
                  {command.argumentHint ? <Text numberOfLines={1} style={styles.entryHint}>{command.argumentHint}</Text> : null}
                </View>
                {command.description ? <Text numberOfLines={1} style={styles.entryDescription}>{command.description}</Text> : null}
              </Pressable>
            ))}
          </View>
        ))}
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create({
  wrap: { paddingBottom: 12, paddingHorizontal: 10 },
  box: { backgroundColor: colors.chromeBg, borderColor: colors.chromeBorder, borderRadius: 16, borderWidth: 1, overflow: "hidden" },
  row: { alignItems: "flex-end", flexDirection: "row", gap: 8, paddingBottom: 7, paddingLeft: 12, paddingRight: 8, paddingTop: 7 },
  input: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.body, maxHeight: 140, minHeight: 34, paddingBottom: 6, paddingTop: 6 },
  mic: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: 17, borderWidth: 1, height: 34, justifyContent: "center", width: 34 },
  send: { alignItems: "center", backgroundColor: colors.accent, borderRadius: 9, height: 34, justifyContent: "center", width: 34 },
  sendEmpty: { backgroundColor: colors.chromeRaised },
  stop: { backgroundColor: colors.danger },
  stopGlyph: { backgroundColor: colors.chromeInkStrong, borderRadius: 2, height: 11, width: 11 },
  disabled: { opacity: 0.4 },
  boxDisabled: { opacity: 0.55 },
  pressed: { opacity: 0.75 },
  banner: { alignItems: "center", backgroundColor: colors.accentWash, borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 8, paddingHorizontal: 12, paddingVertical: 6 },
  bannerText: { color: colors.chromeInk, flex: 1, fontSize: typeScale.meta, lineHeight: 16 },
  bannerCancel: { justifyContent: "center", minHeight: 32, paddingHorizontal: 4 },
  bannerCancelLabel: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  pop: { backgroundColor: colors.chromeRaised, borderColor: colors.chromeBorder, borderRadius: 9, borderWidth: 1, marginBottom: 6, overflow: "hidden" },
  popScroll: { maxHeight: 260 },
  section: { color: colors.chromeFaint, fontSize: 10.5, letterSpacing: 0.8, paddingBottom: 3, paddingHorizontal: 12, paddingTop: 8, textTransform: "uppercase" },
  entry: { justifyContent: "center", minHeight: 44, paddingHorizontal: 12, paddingVertical: 5 },
  entryPressed: { backgroundColor: colors.accentWash },
  entryLine: { alignItems: "baseline", flexDirection: "row", gap: 8 },
  entryName: { color: colors.chromeInkStrong, flexShrink: 1, fontFamily: fonts.mono, fontSize: 12.5 },
  entryHint: { color: colors.chromeFaint, flexShrink: 1, fontSize: 11 },
  entryDescription: { color: colors.chromeDim, fontSize: typeScale.meta },
});
