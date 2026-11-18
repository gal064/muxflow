// The conversation as the phone's list draws it: one row per thing on screen,
// newest first, because the list is inverted (its first row sits at the end of
// the conversation). Pure: nothing here knows about a view.

import { ChatItemKind, type ChatItem } from "../../protocol/gen/chat_pb";
import type { ChatPane, OutgoingMessage } from "./chatModel";

/**
 * One row of the conversation. `key` is stable for as long as the row stands
 * for the same thing, so the list keeps its cell and the reader's place.
 */
export type TimelineRow =
  /** One item from the host, drawn alone. */
  | { kind: "item"; key: string; item: ChatItem }
  /** A message this phone sent that the host has not shown back yet. */
  | { kind: "outgoing"; key: string; message: OutgoingMessage };

/** Whether an item has anything to draw. Thinking with no text and a tool item without its tool do not. */
export function isDrawn(item: ChatItem): boolean {
  switch (item.kind) {
    case ChatItemKind.USER:
    case ChatItemKind.ASSISTANT:
    case ChatItemKind.NOTICE:
      return true;
    case ChatItemKind.REASONING:
      return item.text.trim() !== "";
    case ChatItemKind.TOOL:
      return item.tool !== undefined;
    default:
      return false;
  }
}

// An item that did not change keeps its row, so its cell is not drawn again.
const itemRows = new WeakMap<ChatItem, TimelineRow>();

function itemRow(item: ChatItem): TimelineRow {
  let row = itemRows.get(item);
  if (!row) {
    row = { kind: "item", key: item.id, item };
    itemRows.set(item, row);
  }
  return row;
}

/**
 * The rows for `items` (oldest first, as the pane holds them) and the messages
 * still on their way, newest first. An outgoing message's key is the id its
 * item will carry, so the row stays the same row when the host shows it back.
 */
export function timelineRows(items: readonly ChatItem[], outgoing: readonly OutgoingMessage[]): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const held = new Set<string>();
  for (const item of items) held.add(item.id);
  for (let index = outgoing.length - 1; index >= 0; index -= 1) {
    const message = outgoing[index]!;
    if (!held.has(message.id)) rows.push({ kind: "outgoing", key: message.id, message });
  }
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index]!;
    if (isDrawn(item)) rows.push(itemRow(item));
  }
  return rows;
}

/**
 * What the end of the conversation looks like now: it changes when a row
 * arrives there or the newest row's text grows, and not when earlier messages
 * are loaded.
 */
export function latestMark(rows: readonly TimelineRow[]): string {
  const newest = rows[0];
  if (!newest) return "";
  if (newest.kind === "outgoing") return `${newest.key}:sending`;
  const { item } = newest;
  return `${newest.key}:${item.text.length}:${item.tool?.status ?? ""}:${item.tool?.outputPreview.length ?? ""}`;
}

/** What the top of the conversation says about the messages before it. */
export type OlderState =
  /** Nothing older exists, or the chat is not open. */
  | "none"
  /** Older messages can be loaded; the list asks when the top comes within reach. */
  | "more"
  | "loading"
  /** The last load failed; it is tried again only when asked. */
  | "failed"
  /** The host could not serve every earlier message, and holds no more. */
  | "unavailable";

export function olderState(pane: ChatPane | undefined, failed: boolean): OlderState {
  if (!pane || pane.status === "opening") return "none";
  if (pane.olderCursor !== "") {
    // Only a chat that is being followed can be asked for more.
    if (pane.status !== "open") return "none";
    if (pane.loadingOlder) return "loading";
    return failed ? "failed" : "more";
  }
  return pane.state?.historyComplete === false ? "unavailable" : "none";
}
