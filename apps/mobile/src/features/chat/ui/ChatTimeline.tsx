import { memo, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { FlatList, Keyboard, PixelRatio, Platform, Pressable, StyleSheet, Text, View, type LayoutChangeEvent, type ListRenderItemInfo, type NativeScrollEvent, type NativeSyntheticEvent } from "react-native";

import { ChatItemKind, type ChatItem } from "../../../protocol/gen/chat_pb";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { ChatMarkdown } from "../markdown/ChatMarkdown";
import { isStep } from "../steps";
import { latestMark, type OlderState, type TimelineRow } from "../timelineRows";
import { AttachmentChips } from "./AttachmentChips";
import { ArrowDownIcon } from "./ChatIcons";
import { StepsRow } from "./StepsRow";
import { OpenKeys, TimelineCellContext, TimelineOpenContext, type TimelineOpen } from "./timelineOpen";
import { StepRow } from "./ToolRow";
import type { TimelineMessages } from "./useMessageActions";

/** Within this many dp of the newest message the reader is "at the end": the list follows what arrives. */
export const END_REACH = 80;
/** Further from the end than this, going back to it is a jump; an animation would run past rows not drawn yet. */
const GLIDE_WITHIN = 2400;
/** A position away from the end that no finger put there counts as away once it has lasted this long. */
const AWAY_AFTER_MS = 500;
/**
 * A scroll made to keep a tapped line in place is looked at again after this
 * long. Android takes a scroll before the layout sent with it, and so stops
 * it where the conversation used to end when the row's growth is what makes
 * room for it.
 */
const RESETTLE_AFTER_MS = 50;
/** A row that reports a new height later than this after a tap on it changed for another reason. */
const HOLD_WAIT_MS = 500;
/** A touch on a message that lasts this long is a hold: it opens the message's sheet. */
const HOLD_MS = 350;
/** What a screen reader offers on your message: the sheet a hold opens, and the edit. */
const HOLD_ACTION = { name: "longpress", label: "Message actions" } as const;
const EDIT_ACTION = { name: "activate", label: "Edit and resend" } as const;
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
  /** Holding a message, the message that is outlined, and what goes under the newest finished reply. */
  messages: TimelineMessages;
  /** Drawn after the newest row: voice mode's reply player. */
  tail?: ReactNode;
}

/** The height `view` is laid out at now, where the renderer can say at once. It cannot in a test, or for a view that is gone. */
function heightNow(view: View | null): number | undefined {
  let height: number | undefined;
  view?.measure((_x, _y, _width, measured) => {
    if (measured > 0) height = measured;
  });
  return height;
}

/**
 * The conversation (mobile-chat-mock.html, S1 and S2): your messages on the
 * right, the agent's text unboxed, its steps as plain rows, a run of them
 * folded into one line.
 *
 * It is one inverted list, so its offset 0 is the newest message: the chat
 * opens there and stays there when the keyboard changes the list's height.
 * Rows that arrive, and text that grows, are followed only while the reader
 * is within `END_REACH` of the end; further up, what is being read stays
 * where it is. Earlier messages are added past the list's far end, which
 * moves nothing on screen.
 */
export function ChatTimeline({ rows, older, onLoadOlder, onRetryOlder, canEdit, onEdit, messages, tail }: ChatTimelineProps) {
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
  // Until this time a move of the list is the reader's own: they opened or closed a row.
  const byHandUntil = useRef(0);

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
    else if (moving.current || Date.now() < byHandUntil.current) leave(true);
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
  // Only a move by hand is settled. The glide to the end reports its own
  // stop the same way, and settling that would start it again without end,
  // each start taking the list out from under the reader's finger.
  const settle = useCallback(() => {
    if (!moving.current) return;
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

  // ---- opening a row in place ----
  //
  // The list is inverted, so a row that grows keeps its lower edge and its
  // top, where the line just tapped is, moves up. The reader opened it to
  // read what comes under that line, so the line has to stay: the list is
  // scrolled by exactly what the row grew (or shrank), which leaves the line
  // where it was and unfolds the rest below it.
  //
  // The row's height is read just before the change and again once the change
  // is laid out, before it is on screen. In a test, where nothing is laid
  // out, the heights are the ones the rows report.
  const [keys] = useState(() => ({ groups: new OpenKeys(), rows: new OpenKeys() }));
  const heights = useRef(new Map<string, number>());
  const awaited = useRef<{ key: string; drawn: (height: number | undefined) => void } | undefined>(undefined);
  const resettle = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(resettle.current), []);
  // The row the list passes over, away from the end, when it picks the one to
  // hold still (`minIndexForVisible` below). With a tail the tail is first.
  const passedOver = useRef("");
  passedOver.current = tail ? "" : rows[0]?.key ?? "";

  const keepLine = useCallback((wanted: number, listHolds: boolean) => {
    const target = Math.max(0, wanted);
    // Wherever this leaves the reader, their own tap put them there: past the
    // end's reach they are away at once, and nothing glides them back.
    byHandUntil.current = Date.now() + AWAY_AFTER_MS;
    clearTimeout(resettle.current);
    // The scroll is sent in the next task, after the row's new height has
    // been: both then reach the screen together, the height first. Sent
    // first, the scroll would be taken on the old layout, and the list would
    // pick the row it holds still (below) from there and move a second time
    // when the tapped row grew.
    resettle.current = setTimeout(() => {
      if (!listHolds) list.current?.scrollToOffset({ animated: false, offset: target });
      resettle.current = setTimeout(() => {
        resettle.current = undefined;
        if (dragging.current || moving.current || Math.abs(offset.current - target) <= 1) return;
        list.current?.scrollToOffset({ animated: false, offset: target });
      }, RESETTLE_AFTER_MS);
    }, 0);
  }, []);

  const hold = useCallback((key: string, cell: View | null) => {
    const from = heightNow(cell) ?? heights.current.get(key);
    const start = offset.current;
    // Away from the end, the list itself holds everything but its newest row
    // still when that row changes height. Scrolling as well would move it twice.
    const listHolds = isAway.current && passedOver.current === key;
    const asked = Date.now();
    let settled = false;
    const drawn = (to: number | undefined) => {
      if (settled || to === undefined) return;
      settled = true;
      if (awaited.current?.drawn === drawn) awaited.current = undefined;
      if (from === undefined || Date.now() - asked > HOLD_WAIT_MS || Math.abs(to - from) < 0.5) return;
      keepLine(start + to - from, listHolds);
    };
    awaited.current = { key, drawn };
    return () => drawn(heightNow(cell));
  }, [keepLine]);

  const laidOut = useCallback((key: string, height: number) => {
    heights.current.set(key, height);
    if (awaited.current?.key === key) awaited.current.drawn(height);
  }, []);

  const timeline = useMemo<TimelineOpen>(() => ({ ...keys, hold, laidOut }), [hold, keys, laidOut]);

  const reach = useCallback(() => {
    if (older === "more") onLoadOlder();
  }, [older, onLoadOlder]);

  // The row of the message being edited. The rows after it (nearer the end,
  // so before it in `rows`) are what sending the edit removes: they are dimmed.
  const { editingId } = messages;
  const editingAt = useMemo(() => (editingId ? rows.findIndex((row) => row.kind === "item" && row.key === editingId) : -1), [editingId, rows]);
  // The message being edited is shown, mid-screen: it may be well up the
  // list, and the keyboard the edit opens takes the lower part of the screen,
  // which would leave only the dimmed rows after it in view.
  const editingNow = useRef(editingAt);
  editingNow.current = editingAt;
  useEffect(() => {
    if (!editingId) return undefined;
    const show = () => {
      const index = editingNow.current;
      if (index >= 0) list.current?.scrollToIndex({ animated: true, index, viewPosition: 0.5 });
    };
    show();
    const opened = Keyboard.addListener("keyboardDidShow", show);
    return () => opened.remove();
  }, [editingId]);

  const renderRow = useCallback(({ item, index }: ListRenderItemInfo<TimelineRow>) => (
    <RowView
      canEdit={canEdit}
      dimmed={index < editingAt}
      marked={item.kind === "item" && (item.key === messages.editingId || item.key === messages.heldId)}
      onEdit={onEdit}
      onHold={messages.onHold}
      row={item}
      under={item.kind === "item" && item.key === messages.replyEndId ? messages.replyActions : undefined}
    />
  ), [canEdit, editingAt, messages, onEdit]);

  return (
    <TimelineOpenContext.Provider value={timeline}>
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
          // Only an edited message is scrolled to, and it was on screen when
          // it was held: a row not laid out yet is not worth a guess.
          onScrollToIndexFailed={noScrollToIndex}
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
    </TimelineOpenContext.Provider>
  );
}

function noScrollToIndex(): void {}

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

interface MessageRowProps {
  canEdit: boolean;
  onEdit: (item: ChatItem) => void;
  /** The message was held. */
  onHold: (item: ChatItem) => void;
  /** Outlined: the message being edited, or the one whose sheet is open. */
  marked: boolean;
  /** Drawn under the agent's text: the newest finished reply's actions. */
  under?: ReactNode;
}

const RowView = memo(function RowView({ row, canEdit, onEdit, onHold, marked, dimmed, under }: MessageRowProps & { row: TimelineRow; dimmed: boolean }) {
  const timeline = useContext(TimelineOpenContext);
  const view = useRef<View>(null);
  const cell = useMemo(() => ({ key: row.key, view }), [row.key]);
  // Only a row that can open says how tall it is: a tap on it is what changes that.
  const opens = row.kind === "steps" || (row.kind === "item" && isStep(row.item));
  const onLayout = useCallback((event: LayoutChangeEvent) => timeline.laidOut(row.key, event.nativeEvent.layout.height), [row.key, timeline]);
  return (
    <TimelineCellContext.Provider value={cell}>
      <View onLayout={opens ? onLayout : undefined} ref={view} style={dimmed ? styles.cellDimmed : styles.cell}>
        {row.kind === "outgoing" ? (
          <UserBubble pending>
            <AttachmentChips attachments={row.message.attachments} withText={row.message.text !== ""} />
            {/* Files alone are a message: no empty line under them. */}
            {row.message.text === "" && row.message.attachments?.length ? null : <Text style={styles.userText}>{row.message.text}</Text>}
          </UserBubble>
        ) : row.kind === "steps" ? (
          <StepsRow row={row} />
        ) : (
          <TimelineItem canEdit={canEdit && row.item.rewindable} item={row.item} marked={marked} onEdit={onEdit} onHold={onHold} under={under} />
        )}
      </View>
    </TimelineCellContext.Provider>
  );
});

const TimelineItem = memo(function TimelineItem({ item, canEdit, onEdit, onHold, marked, under }: MessageRowProps & { item: ChatItem }) {
  // The same hold for what scrolls sideways inside the reply, which answers its own touches.
  const hold = useMemo(() => ({ onLongPress: () => onHold(item), delayMs: HOLD_MS }), [item, onHold]);
  switch (item.kind) {
    case ChatItemKind.USER:
      return (
        <UserBubble marked={marked} onEdit={canEdit ? () => onEdit(item) : undefined} onHold={() => onHold(item)}>
          <AttachmentChips attachments={item.attachments} onLongPress={() => onHold(item)} withText={item.text !== ""} />
          {/* Not selectable in place: a hold opens the message's sheet, where Select text is. */}
          {item.text === "" && item.attachments.length > 0 ? null : <Text style={styles.userText}>{item.text}</Text>}
        </UserBubble>
      );
    case ChatItemKind.ASSISTANT:
      // A hold anywhere on the reply opens its sheet. It is not one element to
      // a screen reader, which still reads the reply piece by piece and
      // reaches its links and code buttons.
      return (
        <Pressable accessible={false} delayLongPress={HOLD_MS} onLongPress={() => onHold(item)} style={styles.reply}>
          <View accessibilityLabel={item.streaming ? "Agent, still writing" : undefined} style={styles.assistant}>
            {item.text ? <ChatMarkdown hold={hold} selectable={false} source={item.text} streaming={item.streaming} /> : <Text style={styles.faint}>…</Text>}
          </View>
          {under}
        </Pressable>
      );
    case ChatItemKind.REASONING:
    case ChatItemKind.TOOL:
      return <StepRow item={item} />;
    case ChatItemKind.NOTICE:
      return <Text style={styles.notice}>{item.text}</Text>;
    default:
      return null;
  }
});

/**
 * Your message. A hold opens its sheet. A screen reader has that as an action
 * of the message, and Edit and resend as another when the agent allows it.
 * One still on its way has neither.
 */
function UserBubble({ children, onEdit, onHold, marked = false, pending }: { children: ReactNode; onEdit?: (() => void) | undefined; onHold?: (() => void) | undefined; marked?: boolean; pending?: boolean }) {
  return (
    <View style={styles.userRow}>
      <Pressable
        accessibilityActions={onHold ? (onEdit ? [HOLD_ACTION, EDIT_ACTION] : [HOLD_ACTION]) : undefined}
        accessibilityHint={onHold ? "Hold for message actions" : undefined}
        accessibilityLabel={pending ? "Your message, sending" : undefined}
        delayLongPress={HOLD_MS}
        disabled={!onHold}
        onAccessibilityAction={(event) => (event.nativeEvent.actionName === EDIT_ACTION.name ? onEdit : onHold)?.()}
        onLongPress={onHold}
        style={({ pressed }) => [styles.user, marked && styles.userMarked, pending && styles.userPending, pressed && styles.pressed]}
      >
        {children}
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  wrap: { flex: 1 },
  list: { flex: 1 },
  // The list is drawn turned over, and on Android mirrored too, so its own
  // padding stays the same on both sides and each cell carries the rest.
  content: { paddingVertical: 6 },
  cell: { paddingHorizontal: 12, paddingVertical: 6 },
  // What sending the edit in progress removes.
  cellDimmed: { opacity: 0.45, paddingHorizontal: 12, paddingVertical: 6 },
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
  // The message being edited, or the one whose sheet is open.
  userMarked: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  userText: { color: colors.chromeInkStrong, fontSize: typeScale.body, lineHeight: 20 },
  reply: { alignSelf: "stretch", gap: 2 },
  assistant: { alignSelf: "stretch" },
  faint: { color: colors.chromeFaint, fontSize: typeScale.body },
});
