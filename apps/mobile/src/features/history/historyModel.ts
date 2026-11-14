// The History screen's rows and the Snooze menu's times (docs/design/chat-plan.md
// §1, the History and Snooze sections of chat-mock.html). Pure.

import type { HistoryEntry } from "../../protocol/gen/history_pb";

export type HistoryFilter = "all" | "snoozed";

export function isSnoozed(entry: HistoryEntry): boolean {
  return entry.snoozedUntilUnixMillis > 0n || entry.woke;
}

/**
 * The rows for `filter` and `query`, in the host's order (newest first).
 * Search matches the title, the workspace and the provider, as a fuzzy
 * subsequence; a match inside the title ranks first.
 */
export function historyRows(entries: readonly HistoryEntry[], filter: HistoryFilter, query: string, providerName: (adapterId: string) => string): HistoryEntry[] {
  const shown = filter === "snoozed" ? entries.filter(isSnoozed) : [...entries];
  const needle = query.trim().toLowerCase();
  if (!needle) return shown;
  const scored: { entry: HistoryEntry; rank: number; index: number }[] = [];
  shown.forEach((entry, index) => {
    const fields = [entry.title, entry.workspaceName, providerName(entry.adapterId)];
    const rank = fields.findIndex((field) => fuzzyMatch(field, needle));
    if (rank >= 0) scored.push({ entry, rank, index });
  });
  return scored.sort((a, b) => a.rank - b.rank || a.index - b.index).map((entry) => entry.entry);
}

/** Every character of `needle` appears in `haystack`, in order. */
export function fuzzyMatch(haystack: string, needle: string): boolean {
  const text = haystack.toLowerCase();
  let at = 0;
  for (const ch of needle) {
    if (ch === " ") continue;
    at = text.indexOf(ch, at);
    if (at < 0) return false;
    at += 1;
  }
  return true;
}

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function clock(date: Date): string {
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function startOfDay(ms: number): number {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** "14:05" today, "yesterday", "Mon" within the week, "9 Nov" before. */
export function whenLabel(ms: number, now: number): string {
  const date = new Date(ms);
  const days = Math.round((startOfDay(now) - startOfDay(ms)) / 86_400_000);
  if (days <= 0) return clock(date);
  if (days === 1) return "yesterday";
  if (days < 7) return WEEKDAYS[date.getDay()]!;
  return `${date.getDate()} ${MONTHS[date.getMonth()]}`;
}

/** A wake time: "14:05" today, "tomorrow 09:00", "Mon 09:00" within the week, "9 Nov 09:00" after. */
export function wakeLabel(ms: number, now: number): string {
  const date = new Date(ms);
  const days = Math.round((startOfDay(ms) - startOfDay(now)) / 86_400_000);
  if (days <= 0) return clock(date);
  if (days === 1) return `tomorrow ${clock(date)}`;
  if (days < 7) return `${WEEKDAYS[date.getDay()]} ${clock(date)}`;
  return `${date.getDate()} ${MONTHS[date.getMonth()]} ${clock(date)}`;
}

/** The row's right-hand word: running, woke, its wake time, or when it last ran. */
export function historyMeta(entry: HistoryEntry, now: number): string {
  if (entry.running) return "running";
  if (entry.woke) return "woke";
  if (entry.snoozedUntilUnixMillis > 0n) return wakeLabel(Number(entry.snoozedUntilUnixMillis), now);
  const at = Number(entry.lastActiveAtUnixMillis);
  return at > 0 ? whenLabel(at, now) : "";
}

export interface SnoozeChoice {
  label: string;
  untilMs: number;
}

/** The desktop's default intervals: in 1 hour, in 3 hours, tomorrow at 09:00, Monday at 09:00. */
export function snoozeChoices(now: number): SnoozeChoice[] {
  const tomorrow = new Date(startOfDay(now));
  tomorrow.setDate(tomorrow.getDate() + 1);
  tomorrow.setHours(9, 0, 0, 0);
  const monday = new Date(startOfDay(now));
  // Next Monday, a week out when today is Monday.
  monday.setDate(monday.getDate() + (((8 - monday.getDay()) % 7) || 7));
  monday.setHours(9, 0, 0, 0);
  const choices = [
    { label: "In 1 hour", untilMs: now + 3_600_000 },
    { label: "In 3 hours", untilMs: now + 3 * 3_600_000 },
    { label: "Tomorrow, 09:00", untilMs: tomorrow.getTime() },
  ];
  // On a Sunday, tomorrow is Monday: one row says it.
  if (monday.getTime() !== tomorrow.getTime()) choices.push({ label: "Monday, 09:00", untilMs: monday.getTime() });
  return choices;
}

/** Why an agent cannot be snoozed now, or undefined when it can. */
export function snoozeBlocked(agent: { lifecycle: string; nativeSessionId: string; present: boolean }): string | undefined {
  if (!agent.present) return "This agent has ended.";
  if (agent.lifecycle === "working") return "It can't be snoozed while it's working.";
  if (agent.lifecycle === "blocked") return "It can't be snoozed while it's waiting for you.";
  if (!agent.nativeSessionId) return "This session can't be snoozed yet: the host hasn't identified it.";
  return undefined;
}
