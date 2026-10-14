import { describe, expect, it } from "vitest";
import { describeDuration, describeMinute, describeSchedule } from "./cronText";

describe("cron readings", () => {
  it("reads the schedules people write, and anything else as itself", () => {
    expect(describeSchedule("* * * * *")).toBe("Every minute");
    expect(describeSchedule("*/15 * * * *")).toBe("Every 15 minutes");
    expect(describeSchedule("0 * * * *")).toBe("Every hour");
    expect(describeSchedule("5 * * * *")).toBe("Every hour at :05");
    expect(describeSchedule("0 2 * * *")).toBe("Daily 02:00");
    expect(describeSchedule("0 9 * * 1-5")).toBe("Weekdays 09:00");
    expect(describeSchedule("0 9 * * MON-FRI")).toBe("Weekdays 09:00");
    expect(describeSchedule("0 10 * * sat,sun")).toBe("Weekends 10:00");
    expect(describeSchedule("30 7 * * 1")).toBe("Mondays 07:30");
    expect(describeSchedule("30 7 * * 1,3")).toBe("Mondays and Wednesdays 07:30");
    expect(describeSchedule("0 6 1 * *")).toBe("Monthly on day 1, 06:00");
    for (const unusual of ["0 9 * 1 *", "0,30 9 * * *", "0 9 1 * 1", "nonsense"]) {
      expect(describeSchedule(unusual)).toBe(unusual);
    }
  });

  it("says when a host minute is, relative to today", () => {
    const today = new Date(2026, 9, 13, 12, 0);
    expect(describeMinute("2026-10-13T15:00", today)).toBe("15:00");
    expect(describeMinute("2026-10-14T09:00", today)).toBe("Wed 09:00");
    expect(describeMinute("2026-10-24T07:30", today)).toBe("Sat 24 Oct 07:30");
    expect(describeMinute("garbage", today)).toBe("garbage");
  });

  it("reads durations at the precision a run list needs", () => {
    expect(describeDuration(40_000)).toBe("40s");
    expect(describeDuration(14 * 60_000)).toBe("14m");
    expect(describeDuration(82 * 60_000)).toBe("1h 22m");
  });
});
