import * as Clipboard from "expo-clipboard";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AccessibilityInfo, Platform, Pressable, ScrollView, StyleSheet, Text, TextInput, View, useWindowDimensions } from "react-native";

import { ChatItemKind, type ChatItem } from "../../../protocol/gen/chat_pb";
import { Sheet } from "../../../ui/components/Sheet";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { useAnimationsAllowed } from "../../../ui/useAnimationsAllowed";
import { Spinner } from "../../agents/ui/Spinner";
import { plainText } from "../markdown/plainText";
import type { MessageAction } from "../messageRules";
import { messageTime } from "../messageTime";
import { CheckIcon, CopyIcon, EditIcon, ForkIcon, SelectIcon } from "./MessageIcons";

/** How long Copy message reads "Copied" before the sheet closes. */
export const COPIED_CLOSE_MS = 600;

export interface MessageSheetProps {
  /** The message that was held. Undefined closes the sheet. */
  item: ChatItem | undefined;
  /** Its rows, in order (`messageActions`). */
  actions: readonly MessageAction[];
  /** A fork is on its way: Fork from here shows it, and cannot be pressed again. */
  forking: boolean;
  onEdit: (item: ChatItem) => void;
  onFork: (item: ChatItem) => void;
  onDismiss: () => void;
  /** The sheet has finished closing, where the platform says so. */
  onClosed?: (() => void) | undefined;
}

/**
 * What a hold on a message opens (mobile-chat-mock.html, S5): Copy message
 * with the message's day and time beside it, Select text, and where they
 * apply Edit and resend and Fork from here. Select text turns the sheet into
 * the message's plain text, where a part of it can be selected and copied.
 */
export function MessageSheet({ item, actions: given, forking, onEdit, onFork, onDismiss, onClosed }: MessageSheetProps) {
  // The sheet keeps what it showed while it slides away.
  const last = useRef({ item, actions: given });
  if (item) last.current = { item, actions: given };
  const shown = last.current.item;
  const actions = last.current.actions;

  const [selecting, setSelecting] = useState(false);
  const [copied, setCopied] = useState(false);
  // Every opening starts at the list of actions, also for the message held last time.
  const [opened, setOpened] = useState(item?.id ?? "");
  if (opened !== (item?.id ?? "")) {
    setOpened(item?.id ?? "");
    if (item) {
      setSelecting(false);
      setCopied(false);
    }
  }

  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  const closing = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // A sheet closed by hand is not closed again by the copy it made.
  useEffect(() => () => clearTimeout(closing.current), [opened]);

  const animate = useAnimationsAllowed();
  const { height } = useWindowDimensions();
  if (!shown) return null;

  const copy = () => {
    void Clipboard.setStringAsync(shown.text).then(() => {
      setCopied(true);
      AccessibilityInfo.announceForAccessibility("Copied");
      clearTimeout(closing.current);
      closing.current = setTimeout(() => dismiss.current(), COPIED_CLOSE_MS);
    }).catch(() => {});
  };

  const time = messageTime(shown.atUnixMillis, Date.now());
  const row = (action: MessageAction, index: number): ReactNode => {
    // The first row says when the message was written.
    const when = index === 0 && time ? <Text {...fixedChromeText} numberOfLines={1} style={styles.time}>{time}</Text> : null;
    const final = index === actions.length - 1;
    switch (action) {
      case "copy":
        return (
          <Row icon={copied ? <CheckIcon color={colors.okInk} /> : <CopyIcon color={colors.chromeInkStrong} />} key={action} last={final} onPress={copy} title={copied ? "Copied" : "Copy message"}>
            {when}
          </Row>
        );
      case "select":
        return <Row icon={<SelectIcon color={colors.chromeInkStrong} />} key={action} last={final} onPress={() => setSelecting(true)} title="Select text">{when}</Row>;
      case "edit":
        return (
          <Row icon={<EditIcon color={colors.chromeInkStrong} />} key={action} last={final} onPress={() => onEdit(shown)} sub="Removes everything after it" title="Edit and resend">
            {when}
          </Row>
        );
      case "fork":
        return (
          <Row
            busy={forking}
            icon={<ForkIcon color={forking ? colors.accent : colors.chromeInkStrong} />}
            key={action}
            last={final}
            onPress={() => onFork(shown)}
            // The host copies through the end of the turn, so a fork from the user's own message has its reply too.
            sub={shown.kind === ChatItemKind.USER ? "A new chat with the conversation through the reply to this message" : "A new chat with the conversation up to this message"}
            title="Fork from here"
          >
            {forking ? <Spinner animate={animate} ink={colors.accent} ring={colors.chromeRaised} size={16} /> : when}
          </Row>
        );
    }
  };

  // The agent writes Markdown; your own message is the text you typed.
  const plain = selecting ? (shown.kind === ChatItemKind.ASSISTANT ? plainText(shown.text) : shown.text) : "";
  return (
    <Sheet onClosed={onClosed} onDismiss={onDismiss} title={selecting ? "Select text" : undefined} visible={item !== undefined}>
      {selecting ? (
        <ScrollView style={{ maxHeight: Math.round(height * 0.5) }}>
          {/* iOS selects a part of a text only in a field; Android only in a text. */}
          {Platform.OS === "ios"
            ? <TextInput accessibilityLabel="Message text" editable={false} multiline scrollEnabled={false} style={styles.selectable} value={plain} />
            : <Text selectable style={styles.selectable}>{plain}</Text>}
        </ScrollView>
      ) : (
        <View>{actions.map(row)}</View>
      )}
    </Sheet>
  );
}

function Row({ icon, title, sub, last, busy = false, onPress, children }: { icon: ReactNode; title: string; sub?: string; last: boolean; busy?: boolean; onPress: () => void; children?: ReactNode }) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityState={{ busy, disabled: busy }}
      disabled={busy}
      onPress={onPress}
      style={({ pressed }) => [styles.row, last && styles.rowLast, pressed && styles.pressed]}
    >
      {icon}
      <View style={styles.rowText}>
        <Text numberOfLines={1} style={styles.title}>{title}</Text>
        {sub ? <Text numberOfLines={2} style={styles.sub}>{sub}</Text> : null}
      </View>
      {children}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  row: { alignItems: "center", borderBottomColor: colors.chromeHairline, borderBottomWidth: 1, flexDirection: "row", gap: 12, minHeight: 52, paddingHorizontal: 2, paddingVertical: 8 },
  rowLast: { borderBottomWidth: 0 },
  pressed: { opacity: 0.6 },
  rowText: { flex: 1, gap: 1, minWidth: 0 },
  title: { color: colors.chromeInkStrong, fontSize: typeScale.body, lineHeight: 20 },
  sub: { color: colors.chromeFaint, fontSize: typeScale.meta, lineHeight: 16 },
  time: { color: colors.chromeFaint, flexShrink: 0, fontSize: typeScale.meta, fontVariant: ["tabular-nums"] },
  selectable: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 21, paddingBottom: 8, paddingHorizontal: 2 },
});
