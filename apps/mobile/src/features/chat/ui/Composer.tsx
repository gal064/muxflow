import type { ReactNode, Ref } from "react";
import { Pressable, ScrollView, StyleSheet, Text, TextInput, View, type TextInputInstance } from "react-native";

import type { ChatCommand } from "../../../protocol/gen/chat_pb";
import { ArrowUpIcon } from "../../../ui/components/MediaIcons";
import { colors, fixedChromeText, fonts, typeScale } from "../../../ui/tokens";
import { filesRefusal, type FilesState } from "../attach/attachState";
import type { CommandGroup } from "../chatModel";
import { PlusIcon } from "./AttachIcons";
import { ComposerMic, type ComposerDictation } from "./ComposerMic";

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
  /** The field itself, for whoever has to focus it: starting an edit does. */
  inputRef?: Ref<TextInputInstance> | undefined;
  /** The mic: a tap turns voice mode on (off when `on`), a hold dictates. Absent hides it. */
  mic?: { on: boolean; onTap: () => void; dictation: ComposerDictation | undefined } | undefined;
  /** Attaching files. Absent hides the plus button: the agent takes none. */
  attach?: ComposerAttach | undefined;
  /** A row over the box: the terminal's key chips. The slash list takes its place while it shows. */
  above?: ReactNode;
  /** Said in the empty field instead of the chat's words. */
  placeholder?: string | undefined;
  /** Typed as is: no capitals or corrections, as a shell wants. */
  plain?: boolean | undefined;
  /** The field lost the focus. */
  onBlur?: (() => void) | undefined;
  /** The field's test id, for UI flows that type into it. */
  inputTestID?: string | undefined;
}

export interface ComposerAttach {
  /** The plus button was pressed. */
  onOpen: () => void;
  /** The chips of the files waiting to go, drawn in the box above the field. */
  strip: ReactNode;
  /** Whether those files can go. Send waits while one uploads, and after one failed. */
  files: FilesState;
}

/** The reply field: text, the mic, and Send, which is Stop while the agent works. */
export function Composer(props: ComposerProps) {
  const { draft, onDraft, commands, onPickCommand, working, canSendMidTurn, waiting, sideQuestion, disabled, sending, editing, onCancelEdit, onSend, onStop, stopping = false, onFocus, mic } = props;
  const hasText = draft.trim() !== "";
  const attach = props.attach;
  // A side question goes alone: the files wait for the next message.
  const files = attach && !sideQuestion ? attach.files : "none";
  // Files are a message too, with or without text.
  const hasMessage = hasText || files !== "none";
  // Still on their way, or one that failed: the message they go with waits.
  const filesHeld = filesRefusal(files);
  const showSend = !working || ((canSendMidTurn || sideQuestion) && hasMessage) || editing;
  const blocked = waiting && !canSendMidTurn;
  const sendDisabled = disabled || sending || (blocked && !sideQuestion) || !hasMessage || filesHeld !== undefined;
  // Nothing to send yet, or not all of it on the host yet.
  const sendPlain = !hasMessage || files === "uploading";
  const placeholder = props.placeholder ?? (blocked ? "Answer the card above" : working && canSendMidTurn ? "Send a follow-up while it works" : "Reply");
  // A dictation is sent as Send would send it, so it waits for what Send waits for.
  const dictationRefusal = blocked && !sideQuestion ? "Answer the card above first" : working && !canSendMidTurn && !sideQuestion && !editing ? "The agent takes a message once it stops" : "";
  const dictation = mic?.dictation;
  return (
    <View style={styles.wrap}>
      {commands.length > 0 ? <SlashList groups={commands} onPick={onPickCommand} /> : props.above}
      <View style={[styles.box, disabled && styles.boxDisabled]}>
        {editing ? (
          <View style={styles.banner}>
            <Text style={styles.bannerText}>Editing an earlier message. Sending removes everything after it. Files on disk stay as they are.</Text>
            <Pressable accessibilityRole="button" hitSlop={8} onPress={onCancelEdit} style={styles.bannerCancel}>
              <Text {...fixedChromeText} style={styles.bannerCancelLabel}>Cancel</Text>
            </Pressable>
          </View>
        ) : null}
        {attach ? attach.strip : null}
        <View style={[styles.row, attach && styles.rowAttach]}>
          {attach ? (
            <Pressable accessibilityHint="Photos and files" accessibilityLabel="Attach" accessibilityRole="button" disabled={disabled} hitSlop={6} onPress={attach.onOpen} style={({ pressed }) => [styles.mic, pressed && styles.pressed]}>
              <PlusIcon color={colors.chromeDim} size={18} />
            </Pressable>
          ) : null}
          <TextInput
            accessibilityLabel="Message"
            autoCapitalize={props.plain ? "none" : undefined}
            autoCorrect={props.plain ? false : undefined}
            editable={!disabled}
            multiline
            onBlur={props.onBlur}
            onChangeText={onDraft}
            onFocus={onFocus}
            placeholder={placeholder}
            placeholderTextColor={colors.chromeFaint}
            ref={props.inputRef}
            style={styles.input}
            testID={props.inputTestID}
            value={draft}
          />
          {mic ? <ComposerMic dictation={dictation} disabled={disabled} on={mic.on} onTap={mic.onTap} sendRefusal={dictationRefusal} /> : null}
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
              accessibilityHint={filesHeld}
              accessibilityLabel="Send"
              accessibilityRole="button"
              accessibilityState={{ disabled: sendDisabled }}
              disabled={sendDisabled}
              hitSlop={6}
              onPress={onSend}
              // Nothing typed is a plain grey button, and so is one that waits for a file to upload; any other reason it is off dims it.
              style={({ pressed }) => [styles.send, sendPlain ? styles.sendEmpty : sendDisabled && styles.disabled, pressed && styles.pressed]}
            >
              <ArrowUpIcon color={sendPlain ? colors.chromeFaint : colors.accentInk} size={18} />
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
  // The plus button sits as far from its edge as Send does from the other.
  rowAttach: { paddingLeft: 8 },
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
