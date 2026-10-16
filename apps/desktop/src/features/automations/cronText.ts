/**
 * Plain-English readings of the cron schedules people actually write, and the
 * host-local minutes the helper reports. A schedule this does not recognise
 * reads as itself: the helper, not this file, decides what runs.
 */

const DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const SHORT_DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const SHORT_MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAY_ALIASES: Record<string, number> = { sun: 0, mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6 };

const number = (text: string, max: number): number | undefined => {
  if (!/^\d{1,2}$/.test(text)) return undefined;
  const value = Number(text);
  return value <= max ? value : undefined;
};

const clock = (hour: number, minute: number) => `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;

function weekday(text: string): number | undefined {
  const value = DAY_ALIASES[text.toLowerCase()] ?? number(text, 7);
  return value === undefined ? undefined : value % 7;
}

/** `1-5`, `sat,sun` and single days, as words; undefined for anything else. */
function weekdays(field: string): string | undefined {
  const lower = field.toLowerCase();
  if (lower === "1-5" || lower === "mon-fri") return "Weekdays";
  if (lower === "0,6" || lower === "6,0" || lower === "sat,sun" || lower === "sun,sat" || lower === "6,7") return "Weekends";
  const days = lower.split(",").map(weekday);
  if (days.some((day) => day === undefined)) return undefined;
  const names = (days as number[]).map((day) => `${DAY_NAMES[day]}s`);
  return names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
}

export function describeSchedule(schedule: string): string {
  const fields = schedule.trim().split(/\s+/);
  if (fields.length !== 5) return schedule;
  const [minuteField, hourField, dayField, monthField, weekdayField] = fields;
  if (monthField !== "*") return schedule;
  if (fields.every((field) => field === "*")) return "Every minute";
  const minute = number(minuteField, 59);
  const step = /^\*\/(\d{1,2})$/.exec(minuteField)?.[1];
  if (step && hourField === "*" && dayField === "*" && weekdayField === "*") return `Every ${Number(step)} minutes`;
  if (minute === undefined) return schedule;
  if (hourField === "*" && dayField === "*" && weekdayField === "*") {
    return minute === 0 ? "Every hour" : `Every hour at :${String(minute).padStart(2, "0")}`;
  }
  const hour = number(hourField, 23);
  if (hour === undefined) return schedule;
  const at = clock(hour, minute);
  if (dayField === "*" && weekdayField === "*") return `Daily ${at}`;
  if (dayField === "*") {
    const days = weekdays(weekdayField);
    return days ? `${days} ${at}` : schedule;
  }
  const day = number(dayField, 31);
  if (day !== undefined && weekdayField === "*") return `Monthly on day ${day}, ${at}`;
  return schedule;
}

/** A host-local minute as `YYYY-MM-DDTHH:MM`, parsed without any time zone. */
function parseKey(key: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(key);
  if (!match) return undefined;
  const [, year, month, day, hour, minute] = match.map(Number);
  return { year, month, day, time: clock(hour, minute), weekday: new Date(Date.UTC(year, month - 1, day)).getUTCDay() };
}

/**
 * When a host-local minute is, relative to `today`: the time alone today, a
 * weekday within the week, a date after that.
 */
export function describeMinute(key: string, today = new Date()): string {
  const at = parseKey(key);
  if (!at) return key;
  const then = Date.UTC(at.year, at.month - 1, at.day);
  const now = Date.UTC(today.getFullYear(), today.getMonth(), today.getDate());
  const days = Math.round((then - now) / 86_400_000);
  if (days === 0) return at.time;
  if (days > 0 && days < 7) return `${SHORT_DAYS[at.weekday]} ${at.time}`;
  return `${SHORT_DAYS[at.weekday]} ${at.day} ${SHORT_MONTHS[at.month - 1]} ${at.time}`;
}

/** A duration in milliseconds as `14m`, `1h 22m` or `40s`. */
export function describeDuration(milliseconds: number): string {
  const seconds = Math.max(0, Math.round(milliseconds / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}
