import { memo, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { FlatList, PixelRatio, Platform, Pressable, StyleSheet, Text, View, type ListRenderItemInfo, type NativeScrollEvent, type NativeSyntheticEvent } from "react-native";

import { ChatItemKind, type ChatItem } from "../../../protocol/gen/chat_pb";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { VoiceMarkdown } from "../../voice/VoiceMarkdown";
import { latestMark, type OlderState, type TimelineRow } from "../timelineRows";
import { ArrowDownIcon } from "./ChatIcons";
import { ToolRow } from "./ToolRow";

/** Within this many dp of the newest message the reader is "at the end": the list follows what arrives. */
export const END_REACH = 80;
/** Further from the end than this, going back to it is a jump; an animation would run past rows not drawn yet. */
const GLIDE_WITHIN = 2400;
/** A position away from the end that no finger put there counts as away once it has lasted this long. */
const AWAY_AFTER_MS = 500;
/** `END_REACH` as the list's own follow threshold, which Android compares with a scroll position in physical pixels. */
const FOLLOW_WITHIN = Platform.OS === "android" ? Math.round(END_REACH * PixelRatio.get()) : END_REACH;

export interface ChatTimelineProps {
  /** Newest first (`timelineRows`). */
  rows: TimelineRow[];
  /** What the top of the conversation says about the messages before it. */
  older: OlderState;
  /** The top came within reach while older messages can be loaded. */
  onLoadOlder: () => void;
  /** Retry was pressed after a load failed. */
  onRetryOlder: () => void;
  /** Your messages can be edited and resent. */
  canEdit: boolean;
  onEdit: (item: ChatItem) => void;
  /** Drawn after the newest row: voice mode's reply player. */
  tail?: ReactNode;
}

/**
 * The conversation (mobile-chat-mock.html, S1): your messages on the right,
 * the agent's text unboxed, tools as rows.
 *
 * It is one inverted list, so its offset 0 is the newest message: the chat
 * opens there and stays there when the keyboard changes the list's height.
 * Rows that arrive, and text that grows, are followed only while the reader
 * is within `END_REACH` of the end; further up, what is being read stays
 * where it is. Earlier messages are added past the list's far end, which
 * moves nothing on screen.
 */
export function ChatTimeline({ rows, older, onLoadOlder, onRetryOlder, canEdit, onEdit, tail }: ChatTimelineProps) {
  const list = useRef<FlatList<TimelineRow>>(null);
  // How far the reader is from the newest message, as the list last reported it.
  const offset = useRef(0);
  // A finger is on the list; and a finger moved it, so it may still be coasting.
  const dragging = useRef(false);
  const moving = useRef(false);
  // The reader left the end. A row that arrives there moves the list off its
  // end for the moment it takes to scroll back, which is not the reader
  // leaving: only a move by hand counts at once, anything else if it lasts.
  const [away, setAway] = useState(false);
  const isAway = useRef(false);
  const lasting = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // Something arrived at the end since the reader left it.
  const [unseen, setUnseen] = useState(false);

  const leave = useCallback((left: boolean) => {
    clearTimeout(lasting.current);
    lasting.current = undefined;
    if (isAway.current === left) return;
    isAway.current = left;
    setAway(left);
    if (!left) setUnseen(false);
  }, []);
  useEffect(() => () => clearTimeout(lasting.current), []);

  const onScroll = useCallback((event: NativeSyntheticEvent<NativeScrollEvent>) => {
    const y = event.nativeEvent.contentOffset.y;
    offset.current = y;
    if (y <= END_REACH) leave(false);
    else if (moving.current) leave(true);
    else if (!isAway.current && lasting.current === undefined) {
      lasting.current = setTimeout(() => {
        lasting.current = undefined;
        if (offset.current > END_REACH) leave(true);
      }, AWAY_AFTER_MS);
    }
  }, [leave]);

  const toEnd = useCallback(() => {
    list.current?.scrollToOffset({ animated: offset.current <= GLIDE_WITHIN, offset: 0 });
  }, []);

  // A reader who stops a little short of the end is taken to it, so "at the
  // end" always shows the newest line and not all but its last few.
  const settle = useCallback(() => {
    moving.current = false;
    if (offset.current > 0 && offset.current <= END_REACH) list.current?.scrollToOffset({ animated: true, offset: 0 });
  }, []);
  const onScrollBeginDrag = useCallback(() => {
    dragging.current = true;
    moving.current = true;
  }, []);
  const onScrollEndDrag = useCallback((event: NativeSyntheticEvent<NativeScrollEvent>) => {
    dragging.current = false;
    // A flick keeps moving: `onMomentumScrollEnd` settles it when it stops.
    if (Math.abs(event.nativeEvent.velocity?.y ?? 0) < 0.05) settle();
  }, [settle]);

  const mark = latestMark(rows);
  const seenMark = useRef(mark);
  useEffect(() => {
    if (seenMark.current === mark) return;
    seenMark.current = mark;
    if (isAway.current) setUnseen(true);
    // At the end the list follows by itself. A reader left short of it (an
    // arrival cut the scroll back short) is brought there, unless a finger
    // is on the list.
    else if (offset.current > 0 && !dragging.current) list.current?.scrollToOffset({ animated: true, offset: 0 });
  }, [mark]);

  // Sending a message goes to it, wherever the reader was.
  const sending = rows[0]?.kind === "outgoing" ? rows[0].key : "";
  useEffect(() => {
    if (sending) list.current?.scrollToOffset({ animated: false, offset: 0 });
  }, [sending]);

  const reach = useCallback(() => {
    if (older === "more") onLoadOlder();
  }, [older, onLoadOlder]);

  const renderRow = useCallback(({ item }: ListRenderItemInfo<TimelineRow>) => <RowView canEdit={canEdit} onEdit={onEdit} row={item} />, [canEdit, onEdit]);

  return (
    <View style={styles.wrap}>
      <FlatList
        ListFooterComponent={older === "none" || older === "more" ? null : <OlderRow older={older} onRetry={onRetryOlder} />}
        ListHeaderComponent={tail ? <View style={styles.cell}>{tail}</View> : null}
        contentContainerStyle={styles.content}
        data={rows}
        inverted
        keyExtractor={rowKey}
        keyboardShouldPersistTaps="handled"
        // The list keeps one row where it is on screen when rows nearer the
        // end than it arrive or change height: the first row, counted from
        // `minIndexForVisible`, that reaches into view. At the end that is
        // the newest row, so its text grows in place and a row arriving
        // after it is scrolled to. Away from the end the newest row is
        // passed over, so its text can grow without moving what is read,
        // even when what is read is that row.
        maintainVisibleContentPosition={{ autoscrollToTopThreshold: FOLLOW_WITHIN, minIndexForVisible: away ? 1 : 0 }}
        onEndReached={reach}
        onEndReachedThreshold={1}
        onMomentumScrollEnd={settle}
        onScroll={onScroll}
        onScrollBeginDrag={onScrollBeginDrag}
        onScrollEndDrag={onScrollEndDrag}
        ref={list}
        // The position is held by counting the list's children, which only
        // matches its rows while none of them is detached for being off screen.
        removeClippedSubviews={false}
        renderItem={renderRow}
        style={styles.list}
      />
      {away ? (
        <Pressable
          accessibilityHint={unseen ? "New messages below" : undefined}
          accessibilityLabel="Jump to latest"
          accessibilityRole="button"
          hitSlop={8}
          onPress={() => {
            setUnseen(false);
            toEnd();
          }}
          style={({ pressed }) => [styles.latest, pressed && styles.pressed]}
        >
          <ArrowDownIcon color={colors.chromeInkStrong} size={16} />
          <Text {...fixedChromeText} style={styles.latestLabel}>Latest</Text>
          {unseen ? <View style={styles.latestDot} /> : null}
        </Pressable>
      ) : null}
    </View>
  );
}

function rowKey(row: TimelineRow): string {
  return row.key;
}

/** The line at the top of the conversation about the messages before it. */
function OlderRow({ older, onRetry }: { older: "loading" | "failed" | "unavailable"; onRetry: () => void }) {
  if (older === "failed") {
    return (
      <View style={[styles.cell, styles.olderFailed]}>
        <Text style={styles.notice}>Couldn't load earlier messages.</Text>
        <Pressable accessibilityRole="button" onPress={onRetry} style={({ pressed }) => [styles.retryButton, pressed && styles.pressed]}>
          <Text style={styles.retry}>Retry</Text>
        </Pressable>
      </View>
    );
  }
  return (
    <View style={styles.cell}>
      <Text style={styles.notice}>{older === "loading" ? "Loading earlier messages…" : "Earlier messages are not available."}</Text>
    </View>
  );
}

const RowView = memo(function RowView({ row, canEdit, onEdit }: { row: TimelineRow; canEdit: boolean; onEdit: (item: ChatItem) => void }) {
  return (
    <View style={styles.cell}>
      {row.kind === "outgoing" ? (
        <UserBubble pending>
          <Text style={styles.userText}>{row.message.text}</Text>
        </UserBubble>
      ) : (
        <TimelineItem canEdit={canEdit && row.item.rewindable} item={row.item} onEdit={onEdit} />
      )}
    </View>
  );
});

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
  wrap: { flex: 1 },
  list: { flex: 1 },
  // The list is drawn turned over, and on Android mirrored too, so its own
  // padding stays the same on both sides and each cell carries the rest.
  content: { paddingVertical: 6 },
  cell: { paddingHorizontal: 12, paddingVertical: 6 },
  pressed: { opacity: 0.75 },
  notice: { alignSelf: "center", color: colors.chromeFaint, fontSize: typeScale.meta, textAlign: "center" },
  olderFailed: { alignItems: "center", flexDirection: "row", justifyContent: "center", paddingVertical: 0 },
  // A touch outside the row goes to its neighbours, so the button itself is the full target.
  retryButton: { justifyContent: "center", minHeight: 44, paddingHorizontal: 8 },
  retry: { color: colors.accent, fontSize: typeScale.meta, fontWeight: "600" },
  latest: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: 999,
    borderWidth: 1,
    bottom: 10,
    elevation: 4,
    flexDirection: "row",
    gap: 6,
    minHeight: 32,
    paddingLeft: 10,
    paddingRight: 12,
    position: "absolute",
    right: 10,
    shadowColor: "#000",
    shadowOffset: { height: 3, width: 0 },
    shadowOpacity: 0.35,
    shadowRadius: 10,
  },
  latestLabel: { color: colors.chromeInkStrong, fontSize: typeScale.meta },
  latestDot: { backgroundColor: colors.accent, borderRadius: 4, height: 7, width: 7 },
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
