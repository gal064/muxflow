// The conversation as the phone's list draws it: one row per thing on screen,
// newest first, because the list is inverted (its first row sits at the end of
// the conversation). Which steps fold into one row is the desktop's rule
// (apps/desktop-gpui/src/chat/timeline.rs, `layout`). Pure: nothing here knows
// about a view.

import type { ChatItem, ChatTurnState } from "../../protocol/gen/chat_pb";
import type { ChatPane, OutgoingMessage } from "./chatModel";
import { GROUP_FROM, isBusy, isDrawn, isStep } from "./steps";

/**
 * One row of the conversation. `key` is stable for as long as the row stands
 * for the same thing, so the list keeps its cell and the reader's place.
 */
export type TimelineRow =
  /** One item from the host, drawn alone. */
  | { kind: "item"; key: string; item: ChatItem }
  /**
   * A run of tool calls and thinking between two pieces of text, folded into
   * one line. `items` are its drawn steps, oldest first. `key` is the id of
   * the run's first item, which is also what its open state is kept by.
   * `live`: the run a turn still in progress is adding to.
   */
  | { kind: "steps"; key: string; items: ChatItem[]; live: boolean }
  /** A message this phone sent that the host has not shown back yet. */
  | { kind: "outgoing"; key: string; message: OutgoingMessage };

export type StepsRow = Extract<TimelineRow, { kind: "steps" }>;

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

// A run none of whose steps changed keeps its row too. Kept by the run's first item.
const stepsRows = new WeakMap<ChatItem, StepsRow>();

function stepsRow(first: ChatItem, steps: ChatItem[], live: boolean): StepsRow {
  const held = stepsRows.get(first);
  if (held && held.live === live && held.items.length === steps.length && held.items.every((item, index) => item === steps[index])) return held;
  const row: StepsRow = { kind: "steps", key: first.id, items: steps, live };
  stepsRows.set(first, row);
  return row;
}

/**
 * The rows for `items` (oldest first, as the pane holds them) while the turn
 * is `turn`, and the messages still on their way, newest first. A run of tool
 * calls and thinking between two other items is one row when it has
 * `GROUP_FROM` drawn steps or more, and a row per step otherwise. An outgoing
 * message's key is the id its item will carry, so the row stays the same row
 * when the host shows it back.
 */
export function timelineRows(items: readonly ChatItem[], outgoing: readonly OutgoingMessage[], turn: ChatTurnState): TimelineRow[] {
  // Oldest first while the runs are read, then turned over.
  const rows: TimelineRow[] = [];
  // The run being read: its first item, and those of it that are drawn.
  let first: ChatItem | undefined;
  let steps: ChatItem[] = [];
  const close = (live: boolean) => {
    if (!first) return;
    if (steps.length >= GROUP_FROM) rows.push(stepsRow(first, steps, live));
    else for (const step of steps) rows.push(itemRow(step));
    first = undefined;
    steps = [];
  };
  for (const item of items) {
    if (isStep(item)) {
      first ??= item;
      if (isDrawn(item, turn)) steps.push(item);
      continue;
    }
    close(false);
    if (isDrawn(item, turn)) rows.push(itemRow(item));
  }
  // Only the run at the end is the one a turn in progress adds to.
  close(isBusy(turn));
  const held = new Set<string>();
  for (const item of items) held.add(item.id);
  for (const message of outgoing) {
    if (!held.has(message.id)) rows.push({ kind: "outgoing", key: message.id, message });
  }
  return rows.reverse();
}

/**
 * What the end of the conversation looks like now: it changes when a row
 * arrives there, the newest row's text grows or its newest step changes, and
 * not when earlier messages are loaded.
 */
export function latestMark(rows: readonly TimelineRow[]): string {
  const newest = rows[0];
  if (!newest) return "";
  if (newest.kind === "outgoing") return `${newest.key}:sending`;
  if (newest.kind === "steps") return `${newest.key}:${newest.items.length}:${itemMark(newest.items.at(-1)!)}`;
  return itemMark(newest.item);
}

function itemMark(item: ChatItem): string {
  return `${item.id}:${item.text.length}:${item.tool?.status ?? ""}:${item.tool?.outputPreview.length ?? ""}`;
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
