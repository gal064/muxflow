// When a message was written, as the phone says it (mobile-chat-mock.html,
// S5): the clock alone under the newest reply, and the day with it in a
// message's sheet. Pure: the caller says what time it is now. The clock and
// the date are the device's own (12 or 24 hours, the order of day and month);
// `locale` names one for the tests.

type Millis = bigint | number;

/** Local midnight `days` days from `at`'s day. */
function dayStart(at: Date, days = 0): number {
  return new Date(at.getFullYear(), at.getMonth(), at.getDate() + days).getTime();
}

// Whether a locale's clock has 12 hours. Asked once per locale.
const twelveHours = new Map<string, boolean>();

function hasTwelveHours(locale: string | undefined): boolean {
  const key = locale ?? "";
  let twelve = twelveHours.get(key);
  if (twelve === undefined) {
    twelve = new Intl.DateTimeFormat(locale, { hour: "numeric" }).resolvedOptions().hour12 === true;
    twelveHours.set(key, twelve);
  }
  return twelve;
}

/** "6:01 PM", or "09:05" where the clock has 24 hours. */
export function clockTime(at: Date, locale?: string): string {
  return at.toLocaleTimeString(locale, { hour: hasTwelveHours(locale) ? "numeric" : "2-digit", minute: "2-digit" });
}

/**
 * The day and time of a message: "Today, 6:01 PM", "Yesterday, 9:10 AM",
 * "Nov 12, 6:01 PM", and with its year once that is another one. Empty for an
 * item that carries no time.
 */
export function messageTime(atUnixMillis: Millis, now: number, locale?: string): string {
  const millis = Number(atUnixMillis);
  if (!Number.isFinite(millis) || millis <= 0) return "";
  const at = new Date(millis);
  const today = new Date(now);
  const clock = clockTime(at, locale);
  const day = dayStart(at);
  if (day === dayStart(today)) return `Today, ${clock}`;
  if (day === dayStart(today, -1)) return `Yesterday, ${clock}`;
  const sameYear = at.getFullYear() === today.getFullYear();
  const date = at.toLocaleDateString(locale, sameYear ? { day: "numeric", month: "short" } : { day: "numeric", month: "short", year: "numeric" });
  return `${date}, ${clock}`;
}

/** The time under the newest reply: the clock alone for one written today, the day with it otherwise. */
export function replyTime(atUnixMillis: Millis, now: number, locale?: string): string {
  const said = messageTime(atUnixMillis, now, locale);
  return said.startsWith("Today, ") ? said.slice("Today, ".length) : said;
}
