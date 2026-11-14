import { memo, useState, type ReactNode } from "react";
import { ActivityIndicator, Pressable, StyleSheet, Text, View } from "react-native";

import { ChatItemKind, type ChatItem } from "../../../protocol/gen/chat_pb";
import { colors, typeScale } from "../../../ui/tokens";
import { VoiceMarkdown } from "../../voice/VoiceMarkdown";
import type { OutgoingMessage } from "../chatModel";
import { ToolRow } from "./ToolRow";

export interface ChatTimelineProps {
  items: ChatItem[];
  outgoing: OutgoingMessage[];
  /** Older messages can be loaded. */
  hasOlder: boolean;
  loadingOlder: boolean;
  /** The host could not serve every earlier message. */
  historyIncomplete: boolean;
  /** Your messages can be edited and resent. */
  canEdit: boolean;
  onLoadOlder: () => void;
  onEdit: (item: ChatItem) => void;
}

/** The conversation, oldest first: your messages on the right, the agent's text unboxed, tools as rows. */
export function ChatTimeline({ items, outgoing, hasOlder, loadingOlder, historyIncomplete, canEdit, onLoadOlder, onEdit }: ChatTimelineProps) {
  return (
    <>
      {hasOlder ? (
        <Pressable accessibilityRole="button" disabled={loadingOlder} onPress={onLoadOlder} style={({ pressed }) => [styles.older, pressed && styles.pressed]}>
          {loadingOlder ? <ActivityIndicator color={colors.accent} size="small" /> : <Text style={styles.olderLabel}>Load earlier messages</Text>}
        </Pressable>
      ) : historyIncomplete ? (
        <Text style={styles.notice}>Earlier messages are not available.</Text>
      ) : null}
      {items.map((item) => <TimelineItem canEdit={canEdit && item.rewindable} item={item} key={item.id} onEdit={onEdit} />)}
      {outgoing.map((message) => (
        <UserBubble key={message.id} pending>
          <Text style={styles.userText}>{message.text}</Text>
        </UserBubble>
      ))}
    </>
  );
}

const TimelineItem = memo(function TimelineItem({ item, canEdit, onEdit }: { item: ChatItem; canEdit: boolean; onEdit: (item: ChatItem) => void }) {
  switch (item.kind) {
    case ChatItemKind.USER:
      return (
        <UserBubble onEdit={canEdit ? () => onEdit(item) : undefined}>
          <Text selectable={!canEdit} style={styles.userText}>{item.text}</Text>
        </UserBubble>
      );
    case ChatItemKind.ASSISTANT:
      return (
        <View accessibilityLabel={item.streaming ? "Agent, still writing" : undefined} style={styles.assistant}>
          {item.text ? <VoiceMarkdown source={item.text} /> : <Text style={styles.faint}>…</Text>}
        </View>
      );
    case ChatItemKind.REASONING:
      return <Reasoning text={item.text} />;
    case ChatItemKind.TOOL:
      return item.tool ? <ToolRow tool={item.tool} /> : null;
    case ChatItemKind.NOTICE:
      return <Text style={styles.notice}>{item.text}</Text>;
    default:
      return null;
  }
});

/** Your message; hold it to edit and resend when the agent allows that. */
function UserBubble({ children, onEdit, pending }: { children: ReactNode; onEdit?: (() => void) | undefined; pending?: boolean }) {
  return (
    <View style={styles.userRow}>
      <Pressable
        accessibilityActions={onEdit ? [{ name: "activate", label: "Edit and resend" }] : undefined}
        accessibilityHint={onEdit ? "Hold to edit and resend" : undefined}
        accessibilityLabel={pending ? "Your message, sending" : undefined}
        delayLongPress={350}
        disabled={!onEdit}
        onAccessibilityAction={onEdit}
        onLongPress={onEdit}
        style={({ pressed }) => [styles.user, pending && styles.userPending, pressed && styles.pressed]}
      >
        {children}
      </Pressable>
    </View>
  );
}

/** The agent's thinking: two dim lines, tap for the rest. */
function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  if (!text.trim()) return null;
  return (
    <Pressable
      accessibilityHint={open ? "Collapses it" : "Shows all of it"}
      accessibilityLabel={`Thinking: ${text}`}
      accessibilityRole="button"
      onPress={() => setOpen((value) => !value)}
      style={styles.reasoning}
    >
      <Text numberOfLines={open ? undefined : 2} style={styles.reasoningText}>{text}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  pressed: { opacity: 0.75 },
  older: { alignItems: "center", alignSelf: "center", borderColor: colors.chromeBorder, borderRadius: 999, borderWidth: 1, justifyContent: "center", minHeight: 44, paddingHorizontal: 16 },
  olderLabel: { color: colors.chromeInk, fontSize: typeScale.rowSecondary },
  notice: { alignSelf: "center", color: colors.chromeFaint, fontSize: typeScale.meta, textAlign: "center" },
  userRow: { flexDirection: "row", justifyContent: "flex-end" },
  user: {
    backgroundColor: colors.chromeRaised,
    borderBottomLeftRadius: 12,
    borderBottomRightRadius: 3,
    borderColor: colors.chromeHairline,
    borderTopLeftRadius: 12,
    borderTopRightRadius: 12,
    borderWidth: 1,
    maxWidth: "88%",
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
  userPending: { opacity: 0.6 },
  userText: { color: colors.chromeInkStrong, fontSize: typeScale.body, lineHeight: 20 },
  assistant: { alignSelf: "stretch" },
  faint: { color: colors.chromeFaint, fontSize: typeScale.body },
  reasoning: { borderLeftColor: colors.chromeBorder, borderLeftWidth: 2, paddingLeft: 10 },
  reasoningText: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, fontStyle: "italic", lineHeight: 18 },
});
