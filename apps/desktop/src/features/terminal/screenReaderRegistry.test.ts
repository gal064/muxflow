import { describe, expect, it, vi } from "vitest";
import { readPaneScreen, registerPaneScreenReader } from "./screenReaderRegistry";

describe("the pane screen reader registry", () => {
  it("reads a registered screen with its row budget", () => {
    const read = vi.fn((rows: number) => `screen of ${rows} rows`);
    registerPaneScreenReader("devbox\u0000%1", read);
    expect(readPaneScreen("devbox\u0000%1", 200)).toBe("screen of 200 rows");
    expect(read).toHaveBeenCalledWith(200);
  });

  it("holds one reader per key and answers nothing for unknown ones", () => {
    registerPaneScreenReader("devbox\u0000%1", () => "first");
    registerPaneScreenReader("devbox\u0000%1", () => "second");
    expect(readPaneScreen("devbox\u0000%1", 10)).toBe("second");
    // The same pane id on another host is another screen.
    expect(readPaneScreen("laptop\u0000%1", 10)).toBeUndefined();
  });

  it("unregisters only its own reader, so a remount's cleanup cannot drop its successor", () => {
    const first = () => "first";
    const unregisterFirst = registerPaneScreenReader("devbox\u0000%1", first);
    registerPaneScreenReader("devbox\u0000%1", () => "second");
    unregisterFirst();
    expect(readPaneScreen("devbox\u0000%1", 10)).toBe("second");
  });
});
