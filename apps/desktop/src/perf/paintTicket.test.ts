import { afterEach, describe, expect, it } from "vitest";
import { createPaintTicket } from "./paintTicket";
import { resetPerfProbe } from "./probe";

afterEach(() => resetPerfProbe());

describe("paint ticket disabled path", () => {
  it("reuses one inert ticket instead of allocating per filesystem event", () => {
    const names = ["explorer.externalChangeToPaint"] as const;
    const first = createPaintTicket(names, 1);
    const second = createPaintTicket(names, 2);

    expect(second).toBe(first);
    expect(() => {
      first.expectSurface(9);
      first.afterPaint(() => { throw new Error("disabled callback must not run"); });
      first.abandon();
    }).not.toThrow();
  });
});
