// What can be done with a message of the conversation (mobile-chat-mock.html,
// S5), and where. Which reply carries Copy and Fork, and which item a fork
// names, are the desktop's rules (apps/desktop-gpui/src/chat/timeline.rs,
// `layout` and `end_reply`; pane.rs, `fork_reply`). Pure: nothing here knows
// about a view.

import { ChatItemKind, type ChatItem, type ChatTurnState } from "../../protocol/gen/chat_pb";
import { isBusy } from "./steps";

/** A message that can be held for its actions: yours, or the agent's text. Steps and notices are not. */
export function isMessage(item: ChatItem): boolean {
  return item.kind === ChatItemKind.USER || item.kind === ChatItemKind.ASSISTANT;
}

/**
 * The turn the item at `index` is part of, as `[start, end)`: from your
 * message that began it up to your next one. What the phone holds may begin
 * part-way through a turn, which then starts at the first item held.
 */
function turnAround(items: readonly ChatItem[], index: number): [start: number, end: number] {
  let start = index;
  while (start > 0 && items[start]!.kind !== ChatItemKind.USER) start -= 1;
  let end = index + 1;
  while (end < items.length && items[end]!.kind !== ChatItemKind.USER) end += 1;
  return [start, end];
}

/** The last text of the agent's in `items[start, end)`. */
function lastText(items: readonly ChatItem[], start: number, end: number): ChatItem | undefined {
  for (let at = end - 1; at >= start; at -= 1) {
    if (items[at]!.kind === ChatItemKind.ASSISTANT) return items[at];
  }
  return undefined;
}

/**
 * The item Fork from here names for the message `itemId`: the last text of
 * the reply in that message's turn, since a fork always runs through the end
 * of a turn. Nothing while that turn is still in progress, or when it has no
 * reply to fork from.
 */
export function forkTarget(items: readonly ChatItem[], itemId: string, turn: ChatTurnState): string | undefined {
  const index = items.findIndex((item) => item.id === itemId);
  if (index < 0 || !isMessage(items[index]!)) return undefined;
  const [start, end] = turnAround(items, index);
  // The newest turn is still being added to.
  if (end === items.length && isBusy(turn)) return undefined;
  return lastText(items, start, end)?.id;
}

/**
 * The item that has Copy, Fork and its time under it: the last text of the
 * newest reply, once its turn has ended. Nothing while a turn is in progress,
 * while a message of yours is on its way (the next turn is about to begin),
 * or when the newest message is yours and has no reply.
 */
export function newestReplyEnd(items: readonly ChatItem[], turn: ChatTurnState, sending: boolean): ChatItem | undefined {
  if (items.length === 0 || sending || isBusy(turn)) return undefined;
  const [start, end] = turnAround(items, items.length - 1);
  const text = lastText(items, start, end);
  return text && !text.streaming && text.text !== "" ? text : undefined;
}

export type MessageAction = "copy" | "select" | "edit" | "fork";

/**
 * The rows of a held message's sheet, in the order it lists them. `edit`: the
 * agent can rewind and the chat can be used; `fork`: the message's turn can
 * be forked.
 */
export function messageActions(item: ChatItem, can: { edit: boolean; fork: boolean }): MessageAction[] {
  if (!isMessage(item)) return [];
  const actions: MessageAction[] = [];
  // A message that is only attachments has no text to copy.
  if (item.text !== "") actions.push("copy", "select");
  if (can.edit && item.kind === ChatItemKind.USER && item.rewindable) actions.push("edit");
  if (can.fork) actions.push("fork");
  return actions;
}
