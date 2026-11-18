import { describe, expect, it } from "vitest";

import { clockTime, messageTime, replyTime } from "./messageTime";

// Local times, so the tests say the same in any time zone.
const at = (year: number, month: number, day: number, hour: number, minute: number) => new Date(year, month - 1, day, hour, minute).getTime();
const NOW = at(2026, 11, 18, 18, 30);
// Some locales put a narrow no-break space before AM and PM.
const plain = (said: string) => said.replace(/[  ]/g, " ");

describe("message times", () => {
  it("says the clock as the device does", () => {
    expect(plain(clockTime(new Date(at(2026, 11, 18, 18, 1)), "en-US"))).toBe("6:01 PM");
    expect(clockTime(new Date(at(2026, 11, 18, 18, 1)), "en-GB")).toBe("18:01");
    expect(clockTime(new Date(at(2026, 11, 18, 9, 5)), "en-GB")).toBe("09:05");
  });

  it("names today and yesterday, and dates anything earlier", () => {
    expect(plain(messageTime(at(2026, 11, 18, 18, 1), NOW, "en-US"))).toBe("Today, 6:01 PM");
    expect(messageTime(at(2026, 11, 18, 0, 0), NOW, "en-GB")).toBe("Today, 00:00");
    expect(messageTime(at(2026, 11, 17, 9, 10), NOW, "en-GB")).toBe("Yesterday, 09:10");
    expect(messageTime(at(2026, 11, 17, 23, 59), NOW, "en-GB")).toBe("Yesterday, 23:59");
    expect(messageTime(at(2026, 11, 16, 23, 59), NOW, "en-GB")).toBe("16 Nov, 23:59");
    expect(plain(messageTime(at(2026, 11, 12, 14, 32), NOW, "en-US"))).toBe("Nov 12, 2:32 PM");
  });

  it("goes by the calendar day, not by how many hours ago", () => {
    const justAfterMidnight = at(2026, 11, 18, 0, 5);
    // Ten minutes earlier is yesterday.
    expect(messageTime(at(2026, 11, 17, 23, 55), justAfterMidnight, "en-GB")).toBe("Yesterday, 23:55");
    // Yesterday across the turn of a month and of a year.
    expect(messageTime(at(2026, 10, 31, 12, 0), at(2026, 11, 1, 8, 0), "en-GB")).toBe("Yesterday, 12:00");
    expect(messageTime(at(2025, 12, 31, 12, 0), at(2026, 1, 1, 8, 0), "en-GB")).toBe("Yesterday, 12:00");
  });

  it("adds the year once it is another one", () => {
    expect(messageTime(at(2025, 11, 18, 14, 32), NOW, "en-GB")).toBe("18 Nov 2025, 14:32");
    expect(plain(messageTime(at(2025, 12, 30, 14, 32), at(2026, 1, 2, 8, 0), "en-US"))).toBe("Dec 30, 2025, 2:32 PM");
  });

  it("dates a message from a host whose clock runs ahead", () => {
    expect(messageTime(at(2026, 11, 19, 0, 10), NOW, "en-GB")).toBe("19 Nov, 00:10");
  });

  it("says nothing for an item that carries no time", () => {
    expect(messageTime(0n, NOW, "en-GB")).toBe("");
    expect(messageTime(0, NOW, "en-GB")).toBe("");
    expect(replyTime(0n, NOW, "en-GB")).toBe("");
  });

  it("takes the item's own time, which is a bigint", () => {
    expect(messageTime(BigInt(at(2026, 11, 18, 14, 32)), NOW, "en-GB")).toBe("Today, 14:32");
  });

  it("under a reply: the clock alone today, the day with it otherwise", () => {
    expect(plain(replyTime(at(2026, 11, 18, 18, 1), NOW, "en-US"))).toBe("6:01 PM");
    expect(replyTime(at(2026, 11, 17, 9, 10), NOW, "en-GB")).toBe("Yesterday, 09:10");
    expect(replyTime(at(2026, 11, 12, 14, 32), NOW, "en-GB")).toBe("12 Nov, 14:32");
  });
});
