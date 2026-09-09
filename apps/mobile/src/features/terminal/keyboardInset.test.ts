import { describe, expect, it } from "vitest";

import { terminalKeyboardInset } from "./keyboardInset";

describe("terminal keyboard inset", () => {
  it("ignores an IME height inherited from another terminal", () => {
    expect(terminalKeyboardInset(24, 405, false)).toBe(24);
  });

  it("uses the IME height while this terminal input owns focus", () => {
    expect(terminalKeyboardInset(24, 405, true)).toBe(405);
  });

  it("never drops below the safe-area inset", () => {
    expect(terminalKeyboardInset(24, 0, true)).toBe(24);
  });
});
