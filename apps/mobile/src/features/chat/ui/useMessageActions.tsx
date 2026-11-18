import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { BackHandler, Platform, type TextInput } from "react-native";

import { ChatTurnState, type ChatItem } from "../../../protocol/gen/chat_pb";
import type { ChatPane } from "../chatModel";
import { forkTarget, isMessage, messageActions, newestReplyEnd, type MessageAction } from "../messageRules";
import { MessageSheet } from "./MessageSheet";
import { ReplyActions } from "./ReplyActions";

/**
 * How long after Edit and resend is picked in the sheet the reply field is
 * focused. The sheet is a window of its own: until it has gone, the screen
 * under it cannot open the keyboard. Android does not say when it has; iOS
 * does (`onClosed`), and this is only what it falls back to.
 */
export const FOCUS_AFTER_SHEET_MS = Platform.OS === "ios" ? 700 : 300;

/** What the conversation draws for its messages beyond their text. */
export interface TimelineMessages {
  /** A message was held: its sheet is asked for. */
  onHold: (item: ChatItem) => void;
  /** The message whose sheet is open, or "". It is outlined. */
  heldId: string;
  /** Your message being edited, or "". It is outlined, and every row after it dimmed. */
  editingId: string;
  /** The item that has `replyActions` under it, or "". */
  replyEndId: string;
  /** Copy, Fork from here and the time, for under the newest finished reply. */
  replyActions: ReactNode;
}

export interface MessageActionsInput {
  /** The chat on screen, once it has something to show. */
  pane: ChatPane | undefined;
  /** The chat can be used: connected, open, and its agent running. */
  usable: boolean;
  editing: ChatItem | undefined;
  onEdit: (item: ChatItem) => void;
  onCancelEdit: () => void;
  onBack: () => void;
  /** Forks from an item, and answers whether the fork was opened (a refusal is said by whoever forks). Absent offers no fork. */
  onFork?: ((itemId: string) => Promise<boolean>) | undefined;
  /** The item whose fork is on its way, or "". */
  forking?: string | undefined;
}

export interface MessageActions {
  /** For the reply field: starting an edit focuses it. */
  field: RefObject<TextInput | null>;
  /** Starts editing `item`, and brings the keyboard up. */
  edit: (item: ChatItem) => void;
  /** Back: out of an edit first, and only then out of the chat. */
  back: () => void;
  timeline: TimelineMessages;
  /** The sheet a held message opens. Drawn once, anywhere on the screen. */
  sheet: ReactNode;
}

const NO_ITEMS: readonly ChatItem[] = [];
const NO_ACTIONS: readonly MessageAction[] = [];

/** The chat as the rows' callbacks read it. */
interface Chat {
  items: readonly ChatItem[];
  turn: ChatTurnState;
  canEdit: boolean;
  canFork: boolean;
}

/** The rows of `item`'s sheet. */
function actionsFor(item: ChatItem, chat: Chat): MessageAction[] {
  return messageActions(item, { edit: chat.canEdit, fork: chat.canFork && forkTarget(chat.items, item.id, chat.turn) !== undefined });
}

/**
 * What can be done with the messages of a chat (mobile-chat-mock.html, S5):
 * the sheet a hold opens, the row under the newest finished reply, and
 * editing an earlier message, which takes the reply field, opens the
 * keyboard, and is what Back leaves first.
 */
export function useMessageActions(input: MessageActionsInput): MessageActions {
  const { pane, usable, editing, forking = "" } = input;
  const items = pane?.items ?? NO_ITEMS;
  const turn = pane?.state?.turn ?? ChatTurnState.UNSPECIFIED;
  const sending = (pane?.outgoing.length ?? 0) > 0;
  const capabilities = pane?.catalog?.capabilities;
  const canEdit = capabilities?.rewind === true && usable;
  const canFork = capabilities?.fork === true && usable && input.onFork !== undefined;

  // The newest of everything, for the callbacks the rows keep.
  const now = useRef({ input, items, turn, canEdit, canFork });
  now.current = { input, items, turn, canEdit, canFork };

  // ---- the sheet ----
  const [heldId, setHeldId] = useState("");
  const heldNow = useRef(heldId);
  heldNow.current = heldId;
  // Read from the conversation each time: a reply still being written is copied as it is now.
  const found = heldId ? items.find((item) => item.id === heldId) : undefined;
  const actions = found ? actionsFor(found, now.current) : NO_ACTIONS;
  const held = found && actions.length > 0 ? found : undefined;
  // The message is gone (the conversation was rewound past it), or nothing can be done with it any more.
  useEffect(() => {
    if (heldId && !held) setHeldId("");
  }, [held, heldId]);
  const hold = useCallback((item: ChatItem) => {
    if (isMessage(item) && actionsFor(item, now.current).length > 0) setHeldId(item.id);
  }, []);
  const close = useCallback(() => setHeldId(""), []);

  // ---- editing ----
  const field = useRef<TextInput>(null);
  const focusWanted = useRef(false);
  const focusTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(focusTimer.current), []);
  const focusField = useCallback(() => {
    clearTimeout(focusTimer.current);
    if (!focusWanted.current) return;
    focusWanted.current = false;
    field.current?.focus();
  }, []);
  const edit = useCallback((item: ChatItem) => {
    const fromSheet = heldNow.current !== "";
    setHeldId("");
    now.current.input.onEdit(item);
    focusWanted.current = true;
    if (fromSheet) focusTimer.current = setTimeout(focusField, FOCUS_AFTER_SHEET_MS);
    else focusField();
  }, [focusField]);

  const isEditing = editing !== undefined;
  useEffect(() => {
    if (!isEditing) return undefined;
    // Registered last, so asked first: the edit goes before the screen does.
    const subscription = BackHandler.addEventListener("hardwareBackPress", () => {
      now.current.input.onCancelEdit();
      return true;
    });
    return () => subscription.remove();
  }, [isEditing]);
  const back = useCallback(() => {
    const { editing: edited, onCancelEdit, onBack } = now.current.input;
    if (edited) onCancelEdit();
    else onBack();
  }, []);

  // ---- fork ----
  // Whoever forks says a refusal; here it only matters whether the fork was opened.
  const fork = useCallback(async (target: string): Promise<boolean> => {
    try {
      return (await now.current.input.onFork?.(target)) === true;
    } catch {
      return false;
    }
  }, []);
  // From the sheet, which stays open and shows the fork on its way. It closes
  // when the fork opens; refused, its row can be pressed again.
  const forkFrom = useCallback((item: ChatItem) => {
    const target = forkTarget(now.current.items, item.id, now.current.turn);
    if (!target) return;
    void fork(target).then((opened) => {
      if (opened) setHeldId((current) => (current === item.id ? "" : current));
    });
  }, [fork]);

  // ---- the newest finished reply ----
  const replyEnd = useMemo(() => newestReplyEnd(items, turn, sending), [items, sending, turn]);
  const replyActions = useMemo(() => (replyEnd ? (
    <ReplyActions
      atUnixMillis={replyEnd.atUnixMillis}
      forkDisabled={forking !== ""}
      forking={forking === replyEnd.id}
      onFork={canFork ? () => void fork(replyEnd.id) : undefined}
      text={replyEnd.text}
    />
  ) : null), [canFork, fork, forking, replyEnd]);

  const editingId = editing?.id ?? "";
  const shownHeldId = held?.id ?? "";
  const replyEndId = replyEnd?.id ?? "";
  const timeline = useMemo<TimelineMessages>(
    () => ({ onHold: hold, heldId: shownHeldId, editingId, replyEndId, replyActions }),
    [editingId, hold, replyActions, replyEndId, shownHeldId],
  );

  const sheet = (
    <MessageSheet actions={actions} forking={forking !== ""} item={held} onClosed={focusField} onDismiss={close} onEdit={edit} onFork={forkFrom} />
  );

  return { field, edit, back, timeline, sheet };
}
